//! Backend client using the Cloudflare Workers Fetch API.
//!
//! Uses `worker::Fetch` for subrequests. Response bodies for proxied S3
//! requests are extracted as `ReadableStream` via `web_sys` for zero-copy
//! passthrough — bytes never enter Rust memory.

use crate::body::WorkerBody;
use s3_proxy_core::backend::{BackendClient, BackendRequest, BackendResponse};
use s3_proxy_core::error::ProxyError;
use serde::de::DeserializeOwned;
use worker::{Cache, Fetch};

/// Options for Cache API caching.
pub(crate) struct CacheOptions {
    pub cache_ttl: u32,
    pub cache_key: Option<String>,
}

/// Build the cache key URL for the Cache API.
///
/// When a custom key is provided, it is formatted as `https://cache.internal/{key}`
/// because the Cache API requires valid URLs as keys.
fn cache_key_url(url: &str, opts: &CacheOptions) -> String {
    match &opts.cache_key {
        Some(key) => format!("https://cache.internal/{}", key),
        None => url.to_string(),
    }
}

/// Fetch a URL and deserialize the JSON response body.
///
/// Used by `source_api` for server-to-server calls to the Source Cooperative API.
/// When `cache` is provided, responses are cached using the Cloudflare Workers
/// Cache API with explicit `cache.get()` / `cache.put()` calls.
pub(crate) async fn fetch_json<T: DeserializeOwned>(
    url: &str,
    headers: &[(&str, &str)],
    cache: Option<&CacheOptions>,
) -> Result<T, ProxyError> {
    // Check cache for a hit before making the request.
    let cache_state = if let Some(opts) = cache {
        let key = cache_key_url(url, opts);
        let cf_cache = Cache::default();
        match cf_cache.get(&key, false).await {
            Ok(Some(mut cached)) => {
                if let Ok(text) = cached.text().await {
                    if let Ok(value) = serde_json::from_str(&text) {
                        return Ok(value);
                    }
                }
                // Cache hit but couldn't deserialize — fall through to fetch.
                Some((cf_cache, key))
            }
            _ => Some((cf_cache, key)),
        }
    } else {
        None
    };

    // Build and execute the fetch request.
    let mut req_init = worker::RequestInit::new();
    let worker_headers = worker::Headers::new();
    for (k, v) in headers {
        worker_headers
            .set(k, v)
            .map_err(|e| ProxyError::Internal(format!("failed to set header: {}", e)))?;
    }
    req_init.with_headers(worker_headers);

    let req = worker::Request::new_with_init(url, &req_init)
        .map_err(|e| ProxyError::Internal(format!("failed to create request: {}", e)))?;

    let mut resp = Fetch::Request(req)
        .send()
        .await
        .map_err(|e| ProxyError::BackendError(format!("fetch failed: {}", e)))?;

    let status = resp.status_code();

    let text = resp
        .text()
        .await
        .map_err(|e| ProxyError::Internal(format!("failed to read text: {}", e)))?;

    if status < 200 || status >= 300 {
        return Err(ProxyError::BackendError(format!(
            "API request to {} returned status {}",
            url, status
        )));
    }

    // Cache successful responses via the Cache API.
    if let Some((cf_cache, key)) = cache_state {
        let ttl = cache.unwrap().cache_ttl;
        if let Ok(mut response) = worker::Response::ok(&text) {
            let _ = response
                .headers_mut()
                .set("Cache-Control", &format!("max-age={}", ttl));
            // cache.put is fire-and-forget; ignore errors.
            let _ = cf_cache.put(&key, response).await;
        }
    }

    serde_json::from_str(&text)
        .map_err(|e| ProxyError::Internal(format!("failed to deserialize response: {}", e)))
}

/// Backend client that uses the Workers Fetch API.
///
/// Response bodies remain as opaque JS ReadableStreams — bytes never touch
/// Rust memory for passthrough requests (GET, PUT, etc.).
pub struct WorkerBackendClient;

impl BackendClient for WorkerBackendClient {
    type Body = WorkerBody;

    async fn send_request(
        &self,
        request: BackendRequest<WorkerBody>,
    ) -> Result<BackendResponse<WorkerBody>, ProxyError> {
        tracing::debug!(
            method = %request.method,
            url = %request.url,
            "worker: sending backend request via Fetch API"
        );

        // Build web_sys::Headers directly
        let ws_headers = web_sys::Headers::new()
            .map_err(|e| ProxyError::Internal(format!("failed to create Headers: {:?}", e)))?;

        for (key, value) in request.headers.iter() {
            if let Ok(v) = value.to_str() {
                let _ = ws_headers.set(key.as_str(), v);
            }
        }

        // Build web_sys::RequestInit — we use web_sys types here because
        // WorkerBody needs to pass JS ReadableStream/Uint8Array as the body.
        let init = web_sys::RequestInit::new();
        init.set_method(request.method.as_str());
        init.set_headers(&ws_headers.into());

        // Set body for methods that carry one.
        // Pass streams and bytes through as JS values — no materialization.
        if matches!(request.method, http::Method::PUT | http::Method::POST) {
            if let Some(js_body) = request.body.into_js_body() {
                init.set_body(&js_body);
            }
        }

        let ws_request =
            web_sys::Request::new_with_str_and_init(&request.url, &init).map_err(|e| {
                tracing::error!(error = ?e, "failed to create web_sys::Request");
                ProxyError::BackendError(format!("failed to create request: {:?}", e))
            })?;

        // Convert to worker::Request and fetch via worker::Fetch.
        let worker_req: worker::Request = ws_request.into();
        let worker_resp = Fetch::Request(worker_req).send().await.map_err(|e| {
            tracing::error!(url = %request.url, error = %e, "fetch to backend failed");
            ProxyError::BackendError(format!("fetch failed: {}", e))
        })?;

        let status = worker_resp.status_code();
        tracing::debug!(status = status, "worker: backend response received");

        // Convert back to web_sys::Response to extract ReadableStream body.
        let ws_response: web_sys::Response = worker_resp.into();

        // Convert response headers
        let mut resp_headers = http::HeaderMap::new();
        let response_headers = ws_response.headers();
        for name in &[
            "content-type",
            "content-length",
            "etag",
            "last-modified",
            "x-amz-request-id",
            "x-amz-version-id",
            "accept-ranges",
            "content-range",
        ] {
            if let Ok(Some(value)) = response_headers.get(name) {
                if let Ok(parsed) = value.parse() {
                    resp_headers.insert(*name, parsed);
                }
            }
        }

        // Extract response body as a ReadableStream — zero-copy passthrough.
        let body = WorkerBody::from_ws_response(&ws_response);

        Ok(BackendResponse {
            status,
            headers: resp_headers,
            body,
        })
    }
}
