//! Cloudflare Cache API wrapper for Source Cooperative API responses.
//!
//! Each public function caches one API call type with its own TTL.
//! Adjust the `*_CACHE_SECS` constants to tune per-datatype expiry.

use crate::registry::{DataConnection, SourceProduct, SourceProductList};
use multistore::error::ProxyError;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

/// Percent-encode set for a single URL path segment: everything except the
/// RFC 3986 unreserved characters (`A-Z a-z 0-9 - . _ ~`). Ordinary
/// account/product slugs pass through byte-identical, while a decoded `?`,
/// `#`, `&`, `/`, or space — which the request path is decoded into before
/// segmentation — is encoded so it cannot inject into the upstream API URL or
/// forge a colliding cache key.
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

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
    let api_url = format!(
        "{}/api/v1/products/{}/{}",
        api_base_url,
        utf8_percent_encode(account, PATH_SEGMENT),
        utf8_percent_encode(product, PATH_SEGMENT),
    );
    let cache_key = cache_key_with_subject(&api_url, subject);
    cached_fetch(
        &cache_key,
        &api_url,
        PRODUCT_CACHE_SECS,
        api_auth,
        request_id,
        subject,
        false,
    )
    .await
}

/// Fetch all data connections, cached for `DATA_CONNECTIONS_CACHE_SECS`.
///
/// Pass `force_refresh = true` to bypass the cached copy and refresh it — used
/// when a product references a connection missing from the cached list (e.g. one
/// created after the list cache was filled) so it resolves without waiting out
/// the TTL.
pub async fn get_or_fetch_data_connections(
    api_base_url: &str,
    api_auth: &crate::ApiAuth,
    request_id: &str,
    subject: Option<&str>,
    force_refresh: bool,
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
        force_refresh,
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
    let api_url = format!(
        "{}/api/v1/products/{}",
        api_base_url,
        utf8_percent_encode(account, PATH_SEGMENT),
    );
    let cache_key = cache_key_with_subject(&api_url, subject);
    cached_fetch(
        &cache_key,
        &api_url,
        PRODUCT_LIST_CACHE_SECS,
        api_auth,
        request_id,
        subject,
        false,
    )
    .await
}

// ── Internal helpers ──────────────────────────────────────────────

/// Build a cache key that includes the caller's identity so that
/// responses for different users (or anonymous vs authenticated) are
/// cached separately.
fn cache_key_with_subject(api_url: &str, subject: Option<&str>) -> String {
    match subject {
        // Percent-encode the subject so the cache key is always a well-formed
        // URL. Principal names can contain spaces, `&`, `#`, or non-ASCII, any
        // of which would otherwise corrupt the key or collide distinct subjects.
        // Percent-encoding is injective and URL-safe, so each subject maps to a
        // unique key.
        Some(subj) => {
            let encoded = utf8_percent_encode(subj, PATH_SEGMENT);
            // Pick the query separator defensively. Callers pass query-free
            // URLs today (path segments are percent-encoded above), but a stray
            // `?` must never silently merge into an existing query and forge a
            // cache key that collides with another caller's.
            let sep = if api_url.contains('?') { '&' } else { '?' };
            format!("{api_url}{sep}subject={encoded}")
        }
        None => api_url.to_string(),
    }
}

/// Generic cache-or-fetch: check the Cache API, return cached JSON on hit,
/// otherwise fetch from `api_url`, store in cache with the given TTL, and
/// return the deserialized result.
///
/// `force_refresh` skips the cache read and forces a fresh fetch, still writing
/// the result back so the cached copy is refreshed for later callers.
async fn cached_fetch<T: serde::de::DeserializeOwned>(
    cache_key: &str,
    api_url: &str,
    ttl_secs: u32,
    api_auth: &crate::ApiAuth,
    request_id: &str,
    subject: Option<&str>,
    force_refresh: bool,
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
    if !force_refresh {
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
