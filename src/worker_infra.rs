//! Worker infrastructure: body wrapper, forwarder, and response conversion helpers.
//!
//! Provides the Cloudflare Workers runtime primitives needed by the proxy gateway:
//! - `JsBody` — zero-copy body wrapper around `web_sys::ReadableStream`
//! - `WorkerForwarder` — implements `Forwarder<JsBody>` via the Fetch API
//! - Header and response conversion helpers

use bytes::Bytes;
use http::HeaderMap;
use multistore::error::ProxyError;
use multistore::forwarder::{ForwardResponse, Forwarder};
use multistore::route_handler::{ForwardRequest, ProxyResponseBody, ProxyResult};

use crate::worker_backend::extract_response_headers;

// ── JsBody ──────────────────────────────────────────────────────────

/// Zero-copy body wrapper. Holds the raw `ReadableStream` from the incoming
/// request, passing it through the Gateway untouched for Forward requests.
pub struct JsBody(pub Option<web_sys::ReadableStream>);

// SAFETY: Workers is single-threaded; these are required by Gateway's generic bounds.
unsafe impl Send for JsBody {}
unsafe impl Sync for JsBody {}

// ── WorkerForwarder ─────────────────────────────────────────────────

/// Zero-copy HTTP forwarder for Cloudflare Workers.
///
/// Executes presigned backend requests via the Fetch API, passing
/// `ReadableStream` bodies through JS without touching WASM memory.
pub struct WorkerForwarder;

impl Forwarder<JsBody> for WorkerForwarder {
    type ResponseBody = web_sys::Response;

    async fn forward(
        &self,
        request: ForwardRequest,
        body: JsBody,
    ) -> Result<ForwardResponse<Self::ResponseBody>, ProxyError> {
        // Build web_sys::Headers from the forwarding headers.
        let ws_headers = web_sys::Headers::new()
            .map_err(|e| ProxyError::Internal(format!("failed to create Headers: {:?}", e)))?;
        for (key, value) in request.headers.iter() {
            if let Ok(v) = value.to_str() {
                let _ = ws_headers.set(key.as_str(), v);
            }
        }

        // Build web_sys::RequestInit.
        let init = web_sys::RequestInit::new();
        init.set_method(request.method.as_str());
        init.set_headers(&ws_headers.into());

        // Bypass Cloudflare's subrequest cache for Range requests.
        if request.headers.contains_key(http::header::RANGE) {
            init.set_cache(web_sys::RequestCache::NoStore);
        }

        // For PUT: attach the original ReadableStream directly (zero-copy).
        if request.method == http::Method::PUT {
            if let Some(ref stream) = body.0 {
                init.set_body(stream);
            }
        }

        // Build the outgoing request.
        let ws_request = web_sys::Request::new_with_str_and_init(request.url.as_str(), &init)
            .map_err(|e| ProxyError::Internal(format!("failed to create request: {:?}", e)))?;

        // Fetch via the worker crate's Fetch API.
        let worker_req: worker::Request = ws_request.into();
        let worker_resp = worker::Fetch::Request(worker_req)
            .send()
            .await
            .map_err(|e| ProxyError::BackendError(format!("fetch failed: {}", e)))?;

        // Convert to web_sys::Response to access the body stream.
        let backend_ws: web_sys::Response = worker_resp.into();
        let status = backend_ws.status();

        // Build filtered response headers using the allowlist.
        let headers = extract_response_headers(&backend_ws.headers());
        let content_length = headers
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());

        Ok(ForwardResponse {
            status,
            headers,
            body: backend_ws,
            content_length,
        })
    }
}

// ── Body collection (NeedsBody path) ───────────────────────────────

/// Materialize a `JsBody` into `Bytes` for the NeedsBody path.
///
/// Uses the `Response::arrayBuffer()` JS trick: wrap the stream in a
/// `web_sys::Response`, call `.array_buffer()`, and convert via `Uint8Array`.
/// This is only used for small multipart payloads.
pub async fn collect_js_body(body: JsBody) -> std::result::Result<Bytes, String> {
    match body.0 {
        None => Ok(Bytes::new()),
        Some(stream) => {
            let resp = web_sys::Response::new_with_opt_readable_stream(Some(&stream))
                .map_err(|e| format!("Response::new failed: {:?}", e))?;
            let promise = resp
                .array_buffer()
                .map_err(|e| format!("arrayBuffer() failed: {:?}", e))?;
            let buf = wasm_bindgen_futures::JsFuture::from(promise)
                .await
                .map_err(|e| format!("arrayBuffer await failed: {:?}", e))?;
            let uint8 = js_sys::Uint8Array::new(&buf);
            Ok(Bytes::from(uint8.to_vec()))
        }
    }
}

// ── Response builders ───────────────────────────────────────────────

/// Convert a `ProxyResult` (small buffered XML/JSON) to a `web_sys::Response`.
pub fn proxy_result_to_ws_response(result: ProxyResult) -> web_sys::Response {
    let ws_headers = http_headermap_to_ws_headers(&result.headers)
        .unwrap_or_else(|_| web_sys::Headers::new().unwrap());

    let resp_init = web_sys::ResponseInit::new();
    resp_init.set_status(result.status);
    resp_init.set_headers(&ws_headers.into());

    match result.body {
        ProxyResponseBody::Empty => {
            web_sys::Response::new_with_opt_str_and_init(None, &resp_init).unwrap()
        }
        ProxyResponseBody::Bytes(bytes) => {
            let uint8 = js_sys::Uint8Array::from(bytes.as_ref());
            web_sys::Response::new_with_opt_buffer_source_and_init(Some(&uint8), &resp_init)
                .unwrap()
        }
    }
}

/// Convert a `ForwardResponse<web_sys::Response>` into a `web_sys::Response`
/// for the client, preserving the backend's body stream (zero-copy).
pub fn forward_response_to_ws(resp: ForwardResponse<web_sys::Response>) -> web_sys::Response {
    let ws_headers = http_headermap_to_ws_headers(&resp.headers)
        .unwrap_or_else(|_| web_sys::Headers::new().unwrap());

    let resp_init = web_sys::ResponseInit::new();
    resp_init.set_status(resp.status);
    resp_init.set_headers(&ws_headers.into());

    web_sys::Response::new_with_opt_readable_stream_and_init(resp.body.body().as_ref(), &resp_init)
        .unwrap_or_else(|_| ws_error_response(502, "Bad Gateway"))
}

/// Build a plain-text error response.
pub fn ws_error_response(status: u16, message: &str) -> web_sys::Response {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    web_sys::Response::new_with_opt_str_and_init(Some(message), &init)
        .unwrap_or_else(|_| web_sys::Response::new().unwrap())
}

/// Build an XML response with `content-type: application/xml`.
pub fn ws_xml_response(status: u16, xml_body: &str) -> web_sys::Response {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);

    let headers = web_sys::Headers::new().unwrap();
    let _ = headers.set("content-type", "application/xml");
    init.set_headers(&headers.into());

    web_sys::Response::new_with_opt_str_and_init(Some(xml_body), &init)
        .unwrap_or_else(|_| ws_error_response(500, "Internal Server Error"))
}

// ── Header conversion helpers ───────────────────────────────────────

/// Convert `web_sys::Headers` to `http::HeaderMap` by iterating all entries.
pub fn convert_ws_headers(ws_headers: &web_sys::Headers) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for entry in ws_headers.entries() {
        let Ok(pair) = entry else { continue };
        let arr: js_sys::Array = pair.into();
        let Some(key) = arr.get(0).as_string() else {
            continue;
        };
        let Some(value) = arr.get(1).as_string() else {
            continue;
        };
        let Ok(name) = http::header::HeaderName::from_bytes(key.as_bytes()) else {
            continue;
        };
        let Ok(val) = http::header::HeaderValue::from_str(&value) else {
            continue;
        };
        headers.append(name, val);
    }
    headers
}

/// Convert `http::HeaderMap` to `web_sys::Headers`.
pub fn http_headermap_to_ws_headers(
    headers: &HeaderMap,
) -> std::result::Result<web_sys::Headers, wasm_bindgen::JsValue> {
    let ws = web_sys::Headers::new()?;
    for (key, value) in headers.iter() {
        if let Ok(v) = value.to_str() {
            ws.set(key.as_str(), v)?;
        }
    }
    Ok(ws)
}
