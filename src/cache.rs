//! Cloudflare Cache API wrapper for Source Cooperative API responses.
//!
//! Each public function caches one API call type with its own TTL.
//! Adjust the `*_CACHE_SECS` constants to tune per-datatype expiry.

use crate::registry::{DataConnection, SourceProduct, SourceProductList};
use multistore::error::ProxyError;

// ── Per-datatype TTLs ──────────────────────────────────────────────
// Tune these to control how long each API response is cached at the edge.

/// Product metadata (`/api/v1/products/{account}/{product}`).
const PRODUCT_CACHE_SECS: u32 = 300; // 5 minutes

/// Data connections list (`/api/v1/data-connections`).
const DATA_CONNECTIONS_CACHE_SECS: u32 = 1800; // 30 minutes

/// Product list for an account (`/api/v1/products/{account}`).
const PRODUCT_LIST_CACHE_SECS: u32 = 300; // 5 minutes

// ── Cache key prefix ───────────────────────────────────────────────
// The Cache API requires URL-shaped keys. We use a synthetic domain
// that never hits the network.
const CACHE_KEY_BASE: &str = "https://cache.internal";

// ── Public cache functions ─────────────────────────────────────────

/// Fetch a single product's metadata, cached for `PRODUCT_CACHE_SECS`.
pub async fn get_or_fetch_product(
    api_base_url: &str,
    account: &str,
    product: &str,
) -> Result<SourceProduct, ProxyError> {
    let api_url = format!("{}/api/v1/products/{}/{}", api_base_url, account, product);
    let cache_key = format!("{}/product/{}/{}", CACHE_KEY_BASE, account, product);
    cached_fetch(&cache_key, &api_url, PRODUCT_CACHE_SECS).await
}

/// Fetch all data connections, cached for `DATA_CONNECTIONS_CACHE_SECS`.
pub async fn get_or_fetch_data_connections(
    api_base_url: &str,
) -> Result<Vec<DataConnection>, ProxyError> {
    let api_url = format!("{}/api/v1/data-connections", api_base_url);
    let cache_key = format!("{}/data-connections", CACHE_KEY_BASE);
    cached_fetch(&cache_key, &api_url, DATA_CONNECTIONS_CACHE_SECS).await
}

/// Fetch an account's product list, cached for `PRODUCT_LIST_CACHE_SECS`.
pub async fn get_or_fetch_product_list(
    api_base_url: &str,
    account: &str,
) -> Result<SourceProductList, ProxyError> {
    let api_url = format!("{}/api/v1/products/{}", api_base_url, account);
    let cache_key = format!("{}/product-list/{}", CACHE_KEY_BASE, account);
    cached_fetch(&cache_key, &api_url, PRODUCT_LIST_CACHE_SECS).await
}

// ── Internal helper ────────────────────────────────────────────────

/// Generic cache-or-fetch: check the Cache API, return cached JSON on hit,
/// otherwise fetch from `api_url`, store in cache with the given TTL, and
/// return the deserialized result.
async fn cached_fetch<T: serde::de::DeserializeOwned>(
    cache_key: &str,
    api_url: &str,
    ttl_secs: u32,
) -> Result<T, ProxyError> {
    let cache = worker::Cache::default();

    // ── Cache hit ──────────────────────────────────────────────
    if let Some(mut cached_resp) = cache
        .get(cache_key, false)
        .await
        .map_err(|e| ProxyError::Internal(format!("cache get failed: {}", e)))?
    {
        tracing::debug!("cache hit: {}", cache_key);
        let text = cached_resp
            .text()
            .await
            .map_err(|e| ProxyError::Internal(format!("cache body read failed: {}", e)))?;
        return serde_json::from_str(&text)
            .map_err(|e| ProxyError::Internal(format!("cache JSON parse failed: {}", e)));
    }

    // ── Cache miss — fetch from API ────────────────────────────
    tracing::debug!("cache miss: {} -> {}", cache_key, api_url);
    let req = web_sys::Request::new_with_str(api_url)
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
            status, api_url
        )));
    }

    let text = resp
        .text()
        .await
        .map_err(|e| ProxyError::Internal(format!("body read failed: {}", e)))?;

    // ── Store in cache ─────────────────────────────────────────
    let headers = worker::Headers::new();
    let _ = headers.set("content-type", "application/json");
    let _ = headers.set("cache-control", &format!("max-age={}", ttl_secs));
    if let Ok(cache_resp) = worker::Response::ok(&text) {
        if let Ok(cache_resp) = cache_resp.with_headers(headers) {
            if let Err(e) = cache.put(cache_key, cache_resp).await {
                tracing::warn!("cache put failed: {}", e);
            }
        }
    }

    // ── Deserialize and return ─────────────────────────────────
    serde_json::from_str(&text)
        .map_err(|e| ProxyError::Internal(format!("JSON parse failed: {} for {}", e, api_url)))
}
