//! Custom `BucketRegistry` that resolves product backends via source.coop API.

use multistore::api::response::BucketEntry;
use multistore::error::ProxyError;
use multistore::registry::{BucketRegistry, ResolvedBucket};
use multistore::types::{BucketConfig, ResolvedIdentity, S3Operation};
use serde::Deserialize;
use std::collections::HashMap;

use crate::backend_auth::{apply_backend_auth, BackendAuth};

/// Registry that resolves Source Cooperative products to multistore `BucketConfig`s
/// by calling the Source Cooperative API.
#[derive(Clone)]
pub struct SourceCoopRegistry {
    api_base_url: String,
    api_auth: crate::ApiAuth,
    pub(crate) request_id: String,
}

impl SourceCoopRegistry {
    pub fn new(api_base_url: String, api_auth: crate::ApiAuth, request_id: String) -> Self {
        Self {
            api_base_url,
            api_auth,
            request_id,
        }
    }

    /// Parse "account:product" bucket name into (account, product).
    fn parse_bucket_name(name: &str) -> Option<(&str, &str)> {
        name.split_once(crate::BUCKET_SEPARATOR)
    }

    /// List products for an account via the Source API.
    pub async fn list_products(&self, account: &str) -> Result<Vec<String>, ProxyError> {
        let product_list = crate::cache::get_or_fetch_product_list(
            &self.api_base_url,
            account,
            &self.api_auth,
            &self.request_id,
            None,
        )
        .await?;
        Ok(product_list
            .products
            .into_iter()
            .map(|p| p.product_id)
            .collect())
    }
}

impl BucketRegistry for SourceCoopRegistry {
    async fn get_bucket(
        &self,
        name: &str,
        identity: &ResolvedIdentity,
        _operation: &S3Operation,
    ) -> Result<ResolvedBucket, ProxyError> {
        let (account, product) = Self::parse_bucket_name(name)
            .ok_or_else(|| ProxyError::BucketNotFound(name.to_string()))?;

        let subject = match identity {
            ResolvedIdentity::Authenticated(auth) => Some(auth.principal_name.as_str()),
            ResolvedIdentity::Anonymous => None,
        };

        let config = resolve_product(
            &self.api_base_url,
            account,
            product,
            &self.api_auth,
            &self.request_id,
            subject,
        )
        .await?;

        Ok(ResolvedBucket {
            config,
            list_rewrite: None,
            display_name: None,
        })
    }

    async fn list_buckets(
        &self,
        _identity: &ResolvedIdentity,
    ) -> Result<Vec<BucketEntry>, ProxyError> {
        unimplemented!("Bucket listing is not supported")
    }
}

/// Resolve a product to a `BucketConfig` by querying the Source Cooperative API.
async fn resolve_product(
    api_base_url: &str,
    account: &str,
    product: &str,
    api_auth: &crate::ApiAuth,
    request_id: &str,
    subject: Option<&str>,
) -> Result<BucketConfig, ProxyError> {
    let span = tracing::info_span!(
        "resolve_product",
        account = %account,
        product = %product,
        backend_type = tracing::field::Empty,
    );
    let _guard = span.enter();

    // 1. Fetch product metadata
    let source_product = crate::cache::get_or_fetch_product(
        api_base_url,
        account,
        product,
        api_auth,
        request_id,
        subject,
    )
    .await?;

    // 2. Find primary mirror
    let primary_key = &source_product.metadata.primary_mirror;
    let mirror = source_product
        .metadata
        .mirrors
        .get(primary_key)
        .or_else(|| source_product.metadata.mirrors.values().next())
        .ok_or_else(|| {
            ProxyError::BucketNotFound(format!("no mirrors for {}/{}", account, product))
        })?;

    // 3. Fetch data connections to resolve the actual bucket
    let connections =
        crate::cache::get_or_fetch_data_connections(api_base_url, api_auth, request_id, subject)
            .await?;

    let connection = connections
        .iter()
        .find(|c| c.data_connection_id == mirror.connection_id)
        .ok_or_else(|| {
            ProxyError::Internal(format!(
                "data connection '{}' not found",
                mirror.connection_id
            ))
        })?;

    // 4. Build BucketConfig
    let backend_type = match connection.details.provider.as_str() {
        "s3" => "s3",
        "az" | "azure" => "az",
        "gcs" | "gs" => "gcs",
        other => {
            return Err(ProxyError::Internal(format!(
                "unsupported provider: {}",
                other
            )))
        }
    }
    .to_string();

    let mut backend_options = HashMap::new();

    match backend_type.as_str() {
        "s3" => {
            if let Some(ref bucket) = connection.details.bucket {
                backend_options.insert("bucket_name".to_string(), bucket.clone());
            }
            if let Some(ref region) = connection.details.region {
                backend_options.insert("region".to_string(), region.clone());
                backend_options.insert(
                    "endpoint".to_string(),
                    format!("https://s3.{}.amazonaws.com", region),
                );
            }
        }
        "az" => {
            if let Some(ref account_name) = connection.details.account_name {
                backend_options.insert("account_name".to_string(), account_name.clone());
            }
            if let Some(ref container) = connection.details.container_name {
                backend_options.insert("container_name".to_string(), container.clone());
            }
        }
        _ => {}
    }

    // Backend authentication: unsigned (public) by default, or federate the
    // proxy's OIDC identity into the connection's role. Connections omit
    // `authentication` until a role is configured, so this stays unsigned today.
    apply_backend_auth(
        &connection.authentication,
        &connection.data_connection_id,
        &mut backend_options,
    );

    // 5. Build prefix: connection.base_prefix + mirror.prefix
    let base_prefix = connection.details.base_prefix.as_deref().unwrap_or("");
    let mirror_prefix = &mirror.prefix;
    let full_prefix = format!("{}{}", base_prefix, mirror_prefix);
    let backend_prefix = if full_prefix.is_empty() {
        None
    } else {
        Some(full_prefix)
    };

    let config = BucketConfig {
        name: format!("{}{}{}", account, crate::BUCKET_SEPARATOR, product),
        backend_type,
        backend_prefix,
        anonymous_access: true,
        allowed_roles: vec![],
        backend_options,
    };

    span.record("backend_type", config.backend_type.as_str());
    tracing::debug!(
        prefix = ?config.backend_prefix,
        options = ?config.backend_options,
        "product resolved",
    );

    Ok(config)
}

// ── API response types ─────────────────────────────────────────────

/// Product visibility, mirroring `ProductVisibility` in the source.coop data
/// model. Replaced the legacy `data_mode` field in source.coop#284. Any missing
/// or unrecognized value deserializes to `Unknown`, which is treated as
/// non-public so we fail closed.
#[derive(Debug, Default, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    Unlisted,
    Restricted,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceProduct {
    pub product_id: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub visibility: Visibility,
    pub metadata: SourceProductMetadata,
}

impl SourceProduct {
    pub fn is_public(&self) -> bool {
        !self.disabled && self.visibility == Visibility::Public
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceProductMetadata {
    pub mirrors: HashMap<String, SourceProductMirror>,
    pub primary_mirror: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct SourceProductMirror {
    pub storage_type: String,
    pub connection_id: String,
    pub prefix: String,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DataConnection {
    pub data_connection_id: String,
    pub details: DataConnectionDetails,
    /// How the proxy authenticates to this connection's backend. A sibling of
    /// `details`, matching the Source API's `DataConnection` shape. Absent →
    /// [`BackendAuth::Unsigned`] (public bucket); a present-but-malformed value
    /// becomes `Unsupported` rather than failing the whole list (see
    /// [`deserialize_lenient`](crate::backend_auth::deserialize_lenient)).
    #[serde(default, deserialize_with = "crate::backend_auth::deserialize_lenient")]
    pub authentication: BackendAuth,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DataConnectionDetails {
    pub provider: String,
    pub bucket: Option<String>,
    pub region: Option<String>,
    pub base_prefix: Option<String>,
    pub account_name: Option<String>,
    pub container_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceProductList {
    pub products: Vec<SourceProduct>,
}
