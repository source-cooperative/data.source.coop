//! Backend client and HTTP helpers for the Cloudflare Workers runtime.
//!
//! Contains:
//! - `WorkerBackend` — implements `ProxyBackend` using the Fetch API + FetchConnector
//! - `WorkerHttpClient` — implements `HttpClient` for server-to-server API calls

use crate::fetch_connector::FetchConnector;
use bytes::Bytes;
use http::HeaderMap;
use object_store::signer::Signer;
use object_store::ObjectStore;
use s3_proxy_core::backend::{
    build_object_store, build_signer, ProxyBackend, RawResponse, StoreBuilder,
};
use s3_proxy_core::error::ProxyError;
use s3_proxy_core::types::BucketConfig;
use s3_proxy_source_coop::api::{CacheOptions, HttpClient};
use serde::de::DeserializeOwned;
use std::sync::Arc;
use worker::{Cache, Fetch};

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

/// HTTP client for the Cloudflare Workers runtime.
///
/// Uses the Workers Fetch API for requests and the Cache API for caching.
#[derive(Clone)]
pub struct WorkerHttpClient;

impl HttpClient for WorkerHttpClient {
    async fn fetch_json<T: DeserializeOwned>(
        &self,
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

        if !(200..300).contains(&status) {
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
}

/// Backend for the Cloudflare Workers runtime.
///
/// Uses `FetchConnector` for `object_store` HTTP requests and `web_sys::fetch`
/// for raw multipart operations.
#[derive(Clone)]
pub struct WorkerBackend;

impl ProxyBackend for WorkerBackend {
    fn create_store(&self, config: &BucketConfig) -> Result<Arc<dyn ObjectStore>, ProxyError> {
        build_object_store(config, |b| match b {
            StoreBuilder::S3(s) => StoreBuilder::S3(s.with_http_connector(FetchConnector)),
        })
    }

    fn create_signer(&self, config: &BucketConfig) -> Result<Arc<dyn Signer>, ProxyError> {
        build_signer(config)
    }

    async fn send_raw(
        &self,
        method: http::Method,
        url: String,
        headers: HeaderMap,
        body: Bytes,
    ) -> Result<RawResponse, ProxyError> {
        tracing::debug!(
            method = %method,
            url = %url,
            "worker: sending raw backend request via Fetch API"
        );

        // Build web_sys::Headers
        let ws_headers = web_sys::Headers::new()
            .map_err(|e| ProxyError::Internal(format!("failed to create Headers: {:?}", e)))?;

        for (key, value) in headers.iter() {
            if let Ok(v) = value.to_str() {
                let _ = ws_headers.set(key.as_str(), v);
            }
        }

        // Build web_sys::RequestInit
        let init = web_sys::RequestInit::new();
        init.set_method(method.as_str());
        init.set_headers(&ws_headers.into());

        // Set body for methods that carry one
        if !body.is_empty() {
            let uint8 = js_sys::Uint8Array::from(body.as_ref());
            init.set_body(&uint8.into());
        }

        let ws_request = web_sys::Request::new_with_str_and_init(&url, &init)
            .map_err(|e| ProxyError::BackendError(format!("failed to create request: {:?}", e)))?;

        // Fetch via worker
        let worker_req: worker::Request = ws_request.into();
        let mut worker_resp = Fetch::Request(worker_req)
            .send()
            .await
            .map_err(|e| ProxyError::BackendError(format!("fetch failed: {}", e)))?;

        let status = worker_resp.status_code();

        // Read response body as bytes (multipart responses are small)
        let resp_bytes = worker_resp
            .bytes()
            .await
            .map_err(|e| ProxyError::Internal(format!("failed to read response: {}", e)))?;

        // Convert response headers
        let ws_response: web_sys::Response = worker_resp.into();
        let resp_headers = extract_response_headers(&ws_response.headers());

        Ok(RawResponse {
            status,
            headers: resp_headers,
            body: Bytes::from(resp_bytes),
        })
    }
}

/// Headers to extract from backend responses.
pub const RESPONSE_HEADER_ALLOWLIST: &[&str] = &[
    "content-type",
    "content-length",
    "content-range",
    "etag",
    "last-modified",
    "accept-ranges",
    "content-encoding",
    "content-disposition",
    "cache-control",
    "x-amz-request-id",
    "x-amz-version-id",
    "location",
];

/// Extract response headers from a `web_sys::Headers` using an allowlist.
pub fn extract_response_headers(ws_headers: &web_sys::Headers) -> HeaderMap {
    let mut resp_headers = HeaderMap::new();
    for name in RESPONSE_HEADER_ALLOWLIST {
        if let Ok(Some(value)) = ws_headers.get(name) {
            if let Ok(parsed) = value.parse() {
                resp_headers.insert(*name, parsed);
            }
        }
    }
    resp_headers
}
