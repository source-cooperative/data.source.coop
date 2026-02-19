//! Cloudflare Workers runtime for the S3 proxy gateway.
//!
//! This crate provides implementations of core traits using Cloudflare Workers
//! primitives. The key advantage: response bodies from backend object stores
//! remain as JS `ReadableStream` objects throughout the proxy pipeline, avoiding
//! any conversion to/from Rust byte streams.
//!
//! # Architecture
//!
//! ```text
//! Client -> Worker (JS Request)
//!   -> resolve request (core resolver or Source Cooperative resolver)
//!   -> fetch from backend (JS Fetch API -> JS Response with ReadableStream body)
//!   -> return JS Response with ReadableStream body directly
//! ```
//!
//! The body bytes never touch Rust memory for GET requests. This is the primary
//! performance advantage of the multi-runtime architecture.
//!
//! # Configuration
//!
//! On Workers, configuration is loaded from:
//! - Environment variables / secrets for simple setups
//! - Workers KV for dynamic configuration
//! - The HTTP config provider for centralized config APIs
//! - **Source Cooperative API** when `SOURCE_API_URL` is set

mod body;
mod client;
mod source_api;
mod source_resolver;
mod tracing_layer;

use body::WorkerBody;
use s3_proxy_core::config::static_file::{StaticConfig, StaticProvider};
use s3_proxy_core::proxy::ProxyHandler;
use s3_proxy_core::resolver::DefaultResolver;
use s3_proxy_core::stream::BodyStream;
use worker::*;

/// The Worker entry point.
///
/// Wrangler config (`wrangler.toml`) should bind:
/// - `CONFIG` environment variable or KV namespace for configuration
/// - `VIRTUAL_HOST_DOMAIN` environment variable (optional)
/// - `SOURCE_API_URL` + `SOURCE_API_KEY` for Source Cooperative API mode
#[event(fetch)]
async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    // Initialize panic hook for better error messages
    console_error_panic_hook::set_once();

    // Initialize tracing subscriber (idempotent — ignored if already set)
    tracing::subscriber::set_global_default(tracing_layer::WorkerSubscriber::new()).ok();

    let method = convert_method(&req);
    let url = req.url()?;
    let path = url.path().to_string();
    let query = url.query().map(|q| q.to_string());
    let headers = convert_headers(&req);

    // Extract the request body as a JS ReadableStream — zero CPU cost.
    let body = if matches!(method, http::Method::PUT | http::Method::POST) {
        WorkerBody::from_ws_request(req.inner())
    } else {
        WorkerBody::empty()
    };

    // Source Cooperative API mode: when SOURCE_API_URL is set, resolve backends
    // dynamically from the Source API instead of static PROXY_CONFIG.
    if let Ok(source_api_url) = env.var("SOURCE_API_URL") {
        let source_api_key = env
            .var("SOURCE_API_KEY")
            .map(|v| v.to_string())
            .map_err(|e| {
                worker::Error::RustError(format!(
                    "SOURCE_API_KEY required when SOURCE_API_URL is set: {}",
                    e
                ))
            })?;

        let mut cache_ttls = source_api::CacheTtls::default();
        if let Ok(v) = env.var("SOURCE_CACHE_TTL_PRODUCT") {
            if let Ok(n) = v.to_string().parse::<u32>() {
                cache_ttls.product = n;
            }
        }
        if let Ok(v) = env.var("SOURCE_CACHE_TTL_DATA_CONNECTION") {
            if let Ok(n) = v.to_string().parse::<u32>() {
                cache_ttls.data_connection = n;
            }
        }
        if let Ok(v) = env.var("SOURCE_CACHE_TTL_PERMISSIONS") {
            if let Ok(n) = v.to_string().parse::<u32>() {
                cache_ttls.permissions = n;
            }
        }
        if let Ok(v) = env.var("SOURCE_CACHE_TTL_ACCOUNT") {
            if let Ok(n) = v.to_string().parse::<u32>() {
                cache_ttls.account = n;
            }
        }
        if let Ok(v) = env.var("SOURCE_CACHE_TTL_API_KEY") {
            if let Ok(n) = v.to_string().parse::<u32>() {
                cache_ttls.api_key = n;
            }
        }

        let api_client =
            source_api::SourceApiClient::new(source_api_url.to_string(), source_api_key, cache_ttls);
        let resolver = source_resolver::SourceCoopResolver::new(api_client);
        let handler = ProxyHandler::new(client::WorkerBackendClient, resolver);

        let result = handler
            .handle_request(method, &path, query.as_deref(), &headers, body)
            .await;

        return build_worker_response(result);
    }

    // Load PROXY_CONFIG from environment.
    // Supports two formats:
    //   - JSON string (e.g., set via `wrangler secret` or a plain string var)
    //   - JS object (e.g., set via `[vars.PROXY_CONFIG]` table in wrangler.toml)
    let config = if let Ok(var) = env.var("PROXY_CONFIG") {
        let config_str = var.to_string();
        tracing::debug!(config_len = config_str.len(), "loaded PROXY_CONFIG as string");
        StaticProvider::from_json(&config_str)
            .map_err(|e| worker::Error::RustError(format!("config error: {}", e)))?
    } else {
        tracing::debug!("loading PROXY_CONFIG as object");
        let static_config: StaticConfig = env
            .object_var("PROXY_CONFIG")
            .map_err(|e| worker::Error::RustError(format!("config error: {}", e)))?;
        StaticProvider::from_config(static_config)
    };

    let virtual_host_domain = env.var("VIRTUAL_HOST_DOMAIN").ok().map(|v| v.to_string());
    let resolver = DefaultResolver::new(config, virtual_host_domain);
    let handler = ProxyHandler::new(client::WorkerBackendClient, resolver);

    let result = handler
        .handle_request(method, &path, query.as_deref(), &headers, body)
        .await;

    build_worker_response(result)
}

// ── Shared helpers ──────────────────────────────────────────────────

/// Build a `worker::Response` from a `ProxyResult`, preserving stream bodies.
fn build_worker_response(
    result: s3_proxy_core::proxy::ProxyResult<WorkerBody>,
) -> Result<Response> {
    match result.body {
        WorkerBody::Stream(stream) => {
            let ws_headers = web_sys::Headers::new()
                .map_err(|e| worker::Error::RustError(format!("headers error: {:?}", e)))?;
            for (key, value) in result.headers.iter() {
                if let Ok(v) = value.to_str() {
                    let _ = ws_headers.set(key.as_str(), v);
                }
            }

            let init = web_sys::ResponseInit::new();
            init.set_status(result.status);
            init.set_headers(&ws_headers.into());

            let ws_response =
                web_sys::Response::new_with_opt_readable_stream_and_init(Some(&stream), &init)
                    .map_err(|e| {
                        worker::Error::RustError(format!("failed to build response: {:?}", e))
                    })?;

            Ok(ws_response.into())
        }
        WorkerBody::Bytes(b) => {
            let resp_headers = Headers::new();
            for (key, value) in result.headers.iter() {
                if let Ok(v) = value.to_str() {
                    let _ = resp_headers.set(key.as_str(), v);
                }
            }
            Ok(Response::from_bytes(b.to_vec())?
                .with_status(result.status)
                .with_headers(resp_headers))
        }
        WorkerBody::Empty => {
            let resp_headers = Headers::new();
            for (key, value) in result.headers.iter() {
                if let Ok(v) = value.to_str() {
                    let _ = resp_headers.set(key.as_str(), v);
                }
            }
            Ok(Response::from_bytes(vec![])?
                .with_status(result.status)
                .with_headers(resp_headers))
        }
    }
}

fn convert_method(req: &Request) -> http::Method {
    match req.method() {
        Method::Get => http::Method::GET,
        Method::Head => http::Method::HEAD,
        Method::Post => http::Method::POST,
        Method::Put => http::Method::PUT,
        Method::Delete => http::Method::DELETE,
        _ => http::Method::GET,
    }
}

fn convert_headers(req: &Request) -> http::HeaderMap {
    let mut headers = http::HeaderMap::new();
    for name in &[
        "authorization",
        "host",
        "x-amz-date",
        "x-amz-content-sha256",
        "x-amz-security-token",
        "content-type",
        "content-length",
        "content-md5",
        "range",
    ] {
        if let Ok(Some(value)) = req.headers().get(name) {
            if let Ok(parsed) = value.parse() {
                headers.insert(*name, parsed);
            }
        }
    }
    headers
}
