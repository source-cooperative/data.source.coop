//! Cloudflare Workers runtime for the S3 proxy gateway.
//!
//! This crate provides implementations of core traits using Cloudflare Workers
//! primitives. Uses a two-phase request handling model:
//!
//! 1. **`resolve_request`** determines the action (Forward, Response, NeedsBody)
//! 2. **Forward** requests execute presigned URLs via the Fetch API, passing
//!    JS `ReadableStream` bodies directly — zero Rust stream involvement.
//!
//! # Architecture
//!
//! ```text
//! Client -> Worker (JS Request)
//!   -> resolve request (core resolver or Source Cooperative resolver)
//!   -> Forward: fetch(presigned URL) with ReadableStream passthrough
//!   -> Response: LIST XML via object_store, errors, synthetic responses
//!   -> NeedsBody: multipart operations via raw signed HTTP
//! ```
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
mod fetch_connector;
mod tracing_layer;

use body::build_worker_response;
use client::{extract_response_headers, WorkerBackend};
use s3_proxy_core::config::static_file::{StaticConfig, StaticProvider};
use s3_proxy_core::proxy::{ForwardRequest, HandlerAction, ProxyHandler};
use s3_proxy_core::resolver::{DefaultResolver, RequestResolver};
use s3_proxy_source_coop::api::{CacheTtls, SourceApiClient};
use s3_proxy_source_coop::resolver::SourceCoopResolver;
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

        let mut cache_ttls = CacheTtls::default();
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

        let api_client = SourceApiClient::new(
            client::WorkerHttpClient,
            source_api_url.to_string(),
            source_api_key,
            cache_ttls,
        );
        let resolver = SourceCoopResolver::new(api_client);
        let handler = ProxyHandler::new(WorkerBackend, resolver);

        return handle_action(&req, method, &handler, &path, query.as_deref(), &headers).await;
    }

    // Load PROXY_CONFIG from environment.
    // Supports two formats:
    //   - JSON string (e.g., set via `wrangler secret` or a plain string var)
    //   - JS object (e.g., set via `[vars.PROXY_CONFIG]` table in wrangler.toml)
    let config = if let Ok(var) = env.var("PROXY_CONFIG") {
        let config_str = var.to_string();
        tracing::debug!(
            config_len = config_str.len(),
            "loaded PROXY_CONFIG as string"
        );
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
    let handler = ProxyHandler::new(WorkerBackend, resolver);

    handle_action(&req, method, &handler, &path, query.as_deref(), &headers).await
}

// ── Two-phase request handling ──────────────────────────────────────

/// Handle the resolved action for any resolver type.
async fn handle_action<R: RequestResolver>(
    req: &Request,
    method: http::Method,
    handler: &ProxyHandler<WorkerBackend, R>,
    path: &str,
    query: Option<&str>,
    headers: &http::HeaderMap,
) -> Result<Response> {
    let action = handler
        .resolve_request(method, path, query, headers)
        .await;

    match action {
        HandlerAction::Response(result) => build_worker_response(result),
        HandlerAction::Forward(fwd) => execute_forward(req, fwd).await,
        HandlerAction::NeedsBody(pending) => {
            let body = read_request_body(req).await?;
            let result = handler.handle_with_body(pending, body).await;
            build_worker_response(result)
        }
    }
}

/// Execute a Forward request via the Fetch API.
///
/// For PUT: passes the original JS `ReadableStream` body directly to fetch.
/// For GET: returns the response `ReadableStream` directly to the client.
/// Zero Rust stream involvement — bytes never cross the WASM boundary.
async fn execute_forward(
    req: &Request,
    fwd: ForwardRequest,
) -> Result<Response> {
    // Build request headers
    let ws_headers = web_sys::Headers::new()
        .map_err(|e| worker::Error::RustError(format!("headers error: {:?}", e)))?;

    for (key, value) in fwd.headers.iter() {
        if let Ok(v) = value.to_str() {
            let _ = ws_headers.set(key.as_str(), v);
        }
    }

    // Build fetch request
    let init = web_sys::RequestInit::new();
    init.set_method(fwd.method.as_str());
    init.set_headers(&ws_headers.into());

    // For PUT: pass original request body stream directly
    if fwd.method == http::Method::PUT {
        if let Some(body_stream) = req.inner().body() {
            init.set_body(&body_stream.into());
        }
    }

    let ws_request =
        web_sys::Request::new_with_str_and_init(fwd.url.as_str(), &init)
            .map_err(|e| worker::Error::RustError(format!("request error: {:?}", e)))?;

    // Execute fetch
    let worker_req: worker::Request = ws_request.into();
    let worker_resp = Fetch::Request(worker_req).send().await.map_err(|e| {
        worker::Error::RustError(format!("forward fetch failed: {}", e))
    })?;

    let status = worker_resp.status_code();
    let ws_response: web_sys::Response = worker_resp.into();

    // Forward allowlisted response headers
    let resp_headers = extract_response_headers(&ws_response.headers());
    let ws_resp_headers = web_sys::Headers::new()
        .map_err(|e| worker::Error::RustError(format!("headers error: {:?}", e)))?;
    for (key, value) in resp_headers.iter() {
        if let Ok(v) = value.to_str() {
            let _ = ws_resp_headers.set(key.as_str(), v);
        }
    }

    // Build response with the backend's ReadableStream body (passthrough)
    let resp_init = web_sys::ResponseInit::new();
    resp_init.set_status(status);
    resp_init.set_headers(&ws_resp_headers.into());

    let body = ws_response.body();
    let response = web_sys::Response::new_with_opt_readable_stream_and_init(
        body.as_ref(),
        &resp_init,
    )
    .map_err(|e| worker::Error::RustError(format!("response error: {:?}", e)))?;

    Ok(response.into())
}

// ── Shared helpers ──────────────────────────────────────────────────

/// Read a Worker request body into Bytes.
async fn read_request_body(req: &Request) -> Result<bytes::Bytes> {
    // Extract body as ReadableStream, consume to bytes
    let ws_request = req.inner();
    match ws_request.body() {
        Some(stream) => {
            let response = web_sys::Response::new_with_opt_readable_stream(Some(&stream))
                .map_err(|e| worker::Error::RustError(format!("failed to wrap stream: {:?}", e)))?;

            let array_buffer_promise = response.array_buffer().map_err(|e| {
                worker::Error::RustError(format!("failed to get arrayBuffer: {:?}", e))
            })?;

            let array_buffer = wasm_bindgen_futures::JsFuture::from(array_buffer_promise)
                .await
                .map_err(|e| {
                    worker::Error::RustError(format!("failed to read arrayBuffer: {:?}", e))
                })?;

            let uint8 = js_sys::Uint8Array::new(&array_buffer);
            Ok(bytes::Bytes::from(uint8.to_vec()))
        }
        None => Ok(bytes::Bytes::new()),
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
        "if-match",
        "if-none-match",
        "if-modified-since",
        "if-unmodified-since",
    ] {
        if let Ok(Some(value)) = req.headers().get(name) {
            if let Ok(parsed) = value.parse() {
                headers.insert(*name, parsed);
            }
        }
    }
    headers
}
