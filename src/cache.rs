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
const PRODUCT_LIST_CACHE_SECS: u32 = 60; // 1 minute

// ── Public cache functions ─────────────────────────────────────────

/// Fetch a single product's metadata, cached for `PRODUCT_CACHE_SECS`.
pub async fn get_or_fetch_product(
    api_base_url: &str,
    account: &str,
    product: &str,
    api_auth: &crate::ApiAuth,
    request_id: &str,
    subject: Option<&str>,
) -> Result<SourceProduct, ProxyError> {
    let api_url = format!("{}/api/v1/products/{}/{}", api_base_url, account, product);
    let cache_key = cache_key_with_subject(&api_url, subject);
    cached_fetch(
        &cache_key,
        &api_url,
        PRODUCT_CACHE_SECS,
        api_auth,
        request_id,
        subject,
    )
    .await
}

/// Fetch all data connections, cached for `DATA_CONNECTIONS_CACHE_SECS`.
pub async fn get_or_fetch_data_connections(
    api_base_url: &str,
    api_auth: &crate::ApiAuth,
    request_id: &str,
    subject: Option<&str>,
) -> Result<Vec<DataConnection>, ProxyError> {
    let api_url = format!("{}/api/v1/data-connections", api_base_url);
    let cache_key = cache_key_with_subject(&api_url, subject);
    cached_fetch(
        &cache_key,
        &api_url,
        DATA_CONNECTIONS_CACHE_SECS,
        api_auth,
        request_id,
        subject,
    )
    .await
}

/// Fetch an account's product list, cached for `PRODUCT_LIST_CACHE_SECS`.
pub async fn get_or_fetch_product_list(
    api_base_url: &str,
    account: &str,
    api_auth: &crate::ApiAuth,
    request_id: &str,
    subject: Option<&str>,
) -> Result<SourceProductList, ProxyError> {
    let api_url = format!("{}/api/v1/products/{}", api_base_url, account);
    let cache_key = cache_key_with_subject(&api_url, subject);
    cached_fetch(
        &cache_key,
        &api_url,
        PRODUCT_LIST_CACHE_SECS,
        api_auth,
        request_id,
        subject,
    )
    .await
}

// ── Internal helpers ──────────────────────────────────────────────

/// Build a cache key that includes the caller's identity so that
/// responses for different users (or anonymous vs authenticated) are
/// cached separately.
fn cache_key_with_subject(api_url: &str, subject: Option<&str>) -> String {
    // Appending `?subject=` assumes the URL has no query string of its own —
    // a `?` already present would silently produce a malformed key.
    debug_assert!(
        !api_url.contains('?'),
        "cache_key_with_subject requires a query-free api_url, got {api_url}"
    );
    match subject {
        // Hex-encode the subject so the cache key is always a well-formed URL.
        // Principal names can contain spaces, `&`, `#`, or non-ASCII, any of
        // which would otherwise corrupt the key or collide distinct subjects.
        // Hex is injective and URL-safe, so each subject maps to a unique key.
        Some(subj) => {
            let encoded: String = subj.bytes().map(|b| format!("{:02x}", b)).collect();
            format!("{}?subject={}", api_url, encoded)
        }
        None => api_url.to_string(),
    }
}

/// Generic cache-or-fetch: check the Cache API, return cached JSON on hit,
/// otherwise fetch from `api_url`, store in cache with the given TTL, and
/// return the deserialized result.
async fn cached_fetch<T: serde::de::DeserializeOwned>(
    cache_key: &str,
    api_url: &str,
    ttl_secs: u32,
    api_auth: &crate::ApiAuth,
    request_id: &str,
    subject: Option<&str>,
) -> Result<T, ProxyError> {
    let span = tracing::info_span!(
        "cached_fetch",
        cache_key = %cache_key,
        cache_hit = tracing::field::Empty,
        api_status = tracing::field::Empty,
    );
    let _guard = span.enter();

    let cache = worker::Cache::default();

    // ── Cache hit ──────────────────────────────────────────────
    if let Some(mut cached_resp) = cache
        .get(cache_key, false)
        .await
        .map_err(|e| ProxyError::Internal(format!("cache get failed: {}", e)))?
    {
        span.record("cache_hit", true);
        let text = cached_resp
            .text()
            .await
            .map_err(|e| ProxyError::Internal(format!("cache body read failed: {}", e)))?;
        return serde_json::from_str(&text)
            .map_err(|e| ProxyError::Internal(format!("cache JSON parse failed: {}", e)));
    }

    // ── Cache miss — fetch from API ────────────────────────────
    span.record("cache_hit", false);
    let init = web_sys::RequestInit::new();
    init.set_method("GET");
    let req_headers = web_sys::Headers::new()
        .map_err(|e| ProxyError::Internal(format!("headers build failed: {:?}", e)))?;
    // Only authenticate to the API when we have an identified caller.
    // Anonymous proxy requests hit the API without credentials.
    if let Some(subj) = subject {
        if let Some(auth_value) = api_auth.authorization_header(subj) {
            req_headers
                .set("Authorization", &auth_value)
                .map_err(|e| ProxyError::Internal(format!("header set failed: {:?}", e)))?;
        }
    }
    if !request_id.is_empty() {
        let _ = req_headers.set("x-request-id", request_id);
    }
    init.set_headers(&req_headers);
    let req = web_sys::Request::new_with_str_and_init(api_url, &init)
        .map_err(|e| ProxyError::Internal(format!("request build failed: {:?}", e)))?;
    let worker_req: worker::Request = req.into();
    let mut resp = worker::Fetch::Request(worker_req)
        .send()
        .await
        .map_err(|e| ProxyError::Internal(format!("api fetch failed: {}", e)))?;

    let status = resp.status_code();
    span.record("api_status", status);
    match status {
        200 => {}
        404 => return Err(ProxyError::BucketNotFound("not found".into())),
        // Upstream rejected our credentials (or the resource requires auth we
        // don't have). Surface this as an S3 permissions error rather than a
        // server fault.
        401 | 403 => return Err(ProxyError::AccessDenied),
        _ => {
            return Err(ProxyError::Internal(format!(
                "API returned {} for {}",
                status, api_url
            )))
        }
    }

    let text = resp
        .text()
        .await
        .map_err(|e| ProxyError::Internal(format!("body read failed: {}", e)))?;

    // ── Deserialize first, then cache only on success ──────────
    let result: T = serde_json::from_str(&text)
        .map_err(|e| ProxyError::Internal(format!("JSON parse failed: {} for {}", e, api_url)))?;

    // ── Store in cache ─────────────────────────────────────────
    let headers = worker::Headers::new();
    let _ = headers.set("content-type", "application/json");
    let _ = headers.set("cache-control", &format!("max-age={}", ttl_secs));
    if let Ok(cache_resp) = worker::Response::ok(&text) {
        let cache_resp = cache_resp.with_headers(headers);
        if let Err(e) = cache.put(cache_key, cache_resp).await {
            tracing::warn!("cache put failed: {}", e);
        }
    }

    Ok(result)
}
