//! Custom `BucketRegistry` that resolves product backends via source.coop API.

use multistore::api::response::BucketEntry;
use multistore::error::ProxyError;
use multistore::registry::{BucketRegistry, ResolvedBucket};
use multistore::types::{BucketConfig, ResolvedIdentity, S3Operation};
use serde::Deserialize;
use std::collections::HashMap;

/// Registry that resolves Source Cooperative products to multistore `BucketConfig`s
/// by calling the Source Cooperative API.
#[derive(Clone)]
pub struct SourceCoopRegistry {
    api_base_url: String,
    api_secret: Option<String>,
    request_id: String,
}

impl SourceCoopRegistry {
    pub fn new(api_base_url: String, api_secret: Option<String>, request_id: String) -> Self {
        Self {
            api_base_url,
            api_secret,
            request_id,
        }
    }

    /// Parse "account--product" bucket name into (account, product).
    fn parse_bucket_name(name: &str) -> Option<(&str, &str)> {
        name.split_once("--")
    }

    /// List products for an account via the Source API.
    pub async fn list_products(&self, account: &str) -> Result<Vec<String>, ProxyError> {
        let product_list = crate::cache::get_or_fetch_product_list(
            &self.api_base_url,
            account,
            self.api_secret.as_deref(),
            &self.request_id,
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
        _identity: &ResolvedIdentity,
        _operation: &S3Operation,
    ) -> Result<ResolvedBucket, ProxyError> {
        let (account, product) = Self::parse_bucket_name(name)
            .ok_or_else(|| ProxyError::BucketNotFound(name.to_string()))?;

        let config = resolve_product_send(
            &self.api_base_url,
            account,
            product,
            self.api_secret.as_deref(),
            &self.request_id,
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
        Ok(vec![])
    }
}

/// Resolve a product to a BucketConfig, bridging the !Send worker::Fetch
/// into a Send future via spawn_local + oneshot channel.
async fn resolve_product_send(
    api_base_url: &str,
    account: &str,
    product: &str,
    api_secret: Option<&str>,
    request_id: &str,
) -> Result<BucketConfig, ProxyError> {
    let (tx, rx) = futures::channel::oneshot::channel();
    let api_base_url = api_base_url.to_string();
    let account = account.to_string();
    let product = product.to_string();
    let api_secret = api_secret.map(|s| s.to_string());
    let request_id = request_id.to_string();

    wasm_bindgen_futures::spawn_local(async move {
        let result = resolve_product_inner(
            &api_base_url,
            &account,
            &product,
            api_secret.as_deref(),
            &request_id,
        )
        .await;
        let _ = tx.send(result);
    });

    rx.await
        .unwrap_or_else(|_| Err(ProxyError::Internal("registry channel dropped".into())))
}

/// Inner product resolution logic (runs in spawn_local, !Send is OK).
async fn resolve_product_inner(
    api_base_url: &str,
    account: &str,
    product: &str,
    api_secret: Option<&str>,
    request_id: &str,
) -> Result<BucketConfig, ProxyError> {
    let span = tracing::info_span!(
        "resolve_product",
        account = %account,
        product = %product,
        backend_type = tracing::field::Empty,
    );
    let _guard = span.enter();

    // 1. Fetch product metadata
    let source_product =
        crate::cache::get_or_fetch_product(api_base_url, account, product, api_secret, request_id)
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
        crate::cache::get_or_fetch_data_connections(api_base_url, api_secret, request_id).await?;

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

    // Anonymous access — skip signing
    backend_options.insert("skip_signature".to_string(), "true".to_string());

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
        name: format!("{}--{}", account, product),
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

#[derive(Debug, Clone, Deserialize)]
pub struct SourceProduct {
    pub product_id: String,
    pub metadata: SourceProductMetadata,
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
    pub config: SourceProductMirrorConfig,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct SourceProductMirrorConfig {
    pub region: Option<String>,
    pub bucket: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DataConnection {
    pub data_connection_id: String,
    pub details: DataConnectionDetails,
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
