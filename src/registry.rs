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
}

impl SourceCoopRegistry {
    pub fn new(api_base_url: String) -> Self {
        Self { api_base_url }
    }

    /// Parse "account--product" bucket name into (account, product).
    fn parse_bucket_name(name: &str) -> Option<(&str, &str)> {
        name.split_once("--")
    }

    /// List products for an account via the Source API.
    pub async fn list_products(&self, account: &str) -> Result<Vec<String>, ProxyError> {
        let url = format!("{}/api/v1/products/{}", self.api_base_url, account);
        let product_list: SourceProductList = fetch_json(&url).await?;
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

        let config = resolve_product_send(&self.api_base_url, account, product).await?;

        Ok(ResolvedBucket {
            config,
            list_rewrite: None,
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
) -> Result<BucketConfig, ProxyError> {
    let (tx, rx) = futures::channel::oneshot::channel();
    let api_base_url = api_base_url.to_string();
    let account = account.to_string();
    let product = product.to_string();

    wasm_bindgen_futures::spawn_local(async move {
        let result = resolve_product_inner(&api_base_url, &account, &product).await;
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
) -> Result<BucketConfig, ProxyError> {
    // 1. Fetch product metadata
    let product_url = format!("{}/api/v1/products/{}/{}", api_base_url, account, product);
    let source_product: SourceProduct = fetch_json(&product_url).await?;

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
    let connections_url = format!("{}/api/v1/data-connections", api_base_url);
    let connections: Vec<DataConnection> = fetch_json(&connections_url).await?;

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

    // S3 options
    if let Some(ref bucket) = connection.details.bucket {
        backend_options.insert("bucket_name".to_string(), bucket.clone());
    }
    if let Some(ref region) = connection.details.region {
        backend_options.insert("region".to_string(), region.clone());
        // Construct the S3 endpoint from region
        backend_options.insert(
            "endpoint".to_string(),
            format!("https://s3.{}.amazonaws.com", region),
        );
    }

    // Azure options
    if let Some(ref account_name) = connection.details.account_name {
        backend_options.insert("account_name".to_string(), account_name.clone());
    }
    if let Some(ref container) = connection.details.container_name {
        backend_options.insert("container_name".to_string(), container.clone());
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

    Ok(BucketConfig {
        name: format!("{}--{}", account, product),
        backend_type,
        backend_prefix,
        anonymous_access: true,
        allowed_roles: vec![],
        backend_options,
    })
}

/// HTTP fetch helper using the Workers Fetch API.
async fn fetch_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, ProxyError> {
    let req = web_sys::Request::new_with_str(url)
        .map_err(|e| ProxyError::Internal(format!("request build failed: {:?}", e)))?;
    let worker_req: worker::Request = req.into();
    let mut resp = worker::Fetch::Request(worker_req)
        .send()
        .await
        .map_err(|e| ProxyError::Internal(format!("api fetch failed: {}", e)))?;

    let status = resp.status_code();
    if status == 404 {
        return Err(ProxyError::BucketNotFound("not found".into()));
    }
    if status != 200 {
        return Err(ProxyError::Internal(format!(
            "API returned {} for {}",
            status, url
        )));
    }

    let text = resp
        .text()
        .await
        .map_err(|e| ProxyError::Internal(format!("body read failed: {}", e)))?;
    serde_json::from_str(&text)
        .map_err(|e| ProxyError::Internal(format!("JSON parse failed: {} for {}", e, url)))
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
