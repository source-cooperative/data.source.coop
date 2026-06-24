//! Custom `BucketRegistry` that resolves product backends via source.coop API.

use multistore::api::response::BucketEntry;
use multistore::error::ProxyError;
use multistore::registry::{BucketRegistry, ResolvedBucket};
use multistore::types::{Action, BucketConfig, ResolvedIdentity, S3Operation};
use serde::Deserialize;
use std::collections::HashMap;

use crate::authz::is_write_action;
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
        operation: &S3Operation,
    ) -> Result<ResolvedBucket, ProxyError> {
        // Bucket names arrive pre-mapped as "account:product".
        let (account, product) = name
            .split_once(crate::BUCKET_SEPARATOR)
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
            is_write_action(operation.action()),
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

    async fn authorize_key(
        &self,
        _name: &str,
        _identity: &ResolvedIdentity,
        _action: Action,
        _key: &str,
    ) -> bool {
        // Per-key authorization for batch delete. Source Cooperative authorizes
        // writes at the product level in `get_bucket` (the caller holds product
        // write permission, the connection is writable), so every key in a batch
        // delete that reached this point is permitted. The multistore default
        // would deny every key, since callers' STS sessions carry no per-bucket
        // scopes.
        true
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
    is_write: bool,
) -> Result<BucketConfig, ProxyError> {
    let span = tracing::info_span!(
        "resolve_product",
        account = %account,
        product = %product,
        backend_type = tracing::field::Empty,
        auth_type = tracing::field::Empty,
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

    // 3. Fetch the referenced connection by id, so the subject-scoped API
    // authorizes this exact resource (404/403 → BucketNotFound/AccessDenied)
    // instead of the proxy resolving it out of an over-broad cached list.
    let connection = crate::cache::get_or_fetch_data_connection(
        api_base_url,
        &mirror.connection_id,
        api_auth,
        request_id,
        subject,
    )
    .await?;

    // Authorize writes. The subject-scoped fetches above already cleared the
    // caller to *see* this product; a write additionally requires an
    // authenticated caller who holds the product's `write` permission, a
    // connection that is not read-only, and a connection the proxy can sign as.
    if is_write {
        // Anonymous callers can never write (and there is no subject to query
        // permissions with).
        let subject = subject.ok_or(ProxyError::AccessDenied)?;
        // Connection-level denials need no caller lookup — check them first so a
        // write the connection can't accept skips the permissions API call. A
        // connection can sign writes only via an S3 web-identity role.
        let signable = matches!(
            connection.authentication,
            BackendAuth::S3WebIdentityRole { .. }
        );
        if connection.read_only || !signable {
            return Err(ProxyError::AccessDenied);
        }
        // The caller must hold the product's `write` permission.
        let permissions = crate::cache::get_or_fetch_permissions(
            api_base_url,
            account,
            product,
            api_auth,
            request_id,
            subject,
        )
        .await?;
        if !permissions.iter().any(|p| p == "write") {
            return Err(ProxyError::AccessDenied);
        }
    }

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
    // proxy's OIDC identity into the connection's role.
    //
    // The confused-deputy guard is upstream: the subject-scoped Source API
    // fetches above (get_or_fetch_product / get_or_fetch_data_connection, keyed
    // on the caller's principal) only return the product/connection this caller
    // is authorized for — so reaching here means the caller is already cleared
    // for this connection's backend. Federation does not re-authorize.
    //
    // This ordering is enforced by data dependency, not just statement order:
    // apply_backend_auth needs `connection`, which only exists once the
    // subject-scoped fetch succeeds. A 403/404 from that fetch propagates via `?`
    // before we ever get here, so an unauthorized caller can never reach
    // federation. Guarded end-to-end by tests/test_federation.py's
    // test_restricted_product_denied_to_anonymous.
    span.record("auth_type", connection.authentication.kind());
    apply_backend_auth(
        &connection.authentication,
        &connection.data_connection_id,
        &backend_type,
        &mut backend_options,
    )?;

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
        // Proxy-client-facing: Source Cooperative authorizes callers via its own
        // JWT (enforced upstream at the subject-scoped fetch), not S3 request
        // signing to the proxy — so the proxy accepts anonymous S3 requests and
        // does its own authz. Always true, including for private/federated
        // connections (whose backend auth is handled separately above).
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
pub struct SourceProductMirror {
    pub connection_id: String,
    pub prefix: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DataConnection {
    pub data_connection_id: String,
    /// Whether the connection forbids writes. Required (no serde default): an
    /// absent flag fails the fetch rather than defaulting to writable.
    pub read_only: bool,
    pub details: DataConnectionDetails,
    /// How the proxy authenticates to this connection's backend. A sibling of
    /// `details`, matching the Source API's `DataConnection` shape. Absent →
    /// [`BackendAuth::Unsigned`] (public bucket); a present-but-malformed value
    /// becomes `Unsupported` (fail closed) rather than erroring the fetch (see
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
