//! Response body conversion for the Cloudflare Workers runtime.
//!
//! Converts [`ProxyResult`] to `worker::Response`. Only handles non-streaming
//! bodies (Bytes, Empty). Streaming responses go through the Forward path
//! in `lib.rs`, which uses the Fetch API directly.

use s3_proxy_core::proxy::ProxyResult;
use s3_proxy_core::response_body::ProxyResponseBody;
use worker::{Headers, Response};

/// Build a `worker::Response` from a `ProxyResult`.
///
/// Only handles `Bytes` and `Empty` bodies (LIST XML, errors, multipart XML).
/// Streaming Forward responses are built directly in `lib.rs`.
pub fn build_worker_response(result: ProxyResult) -> Result<Response, worker::Error> {
    let resp_headers = Headers::new();
    for (key, value) in result.headers.iter() {
        if let Ok(v) = value.to_str() {
            let _ = resp_headers.set(key.as_str(), v);
        }
    }

    match result.body {
        ProxyResponseBody::Bytes(b) => Ok(Response::from_bytes(b.to_vec())?
            .with_status(result.status)
            .with_headers(resp_headers)),
        ProxyResponseBody::Empty => Ok(Response::from_bytes(vec![])?
            .with_status(result.status)
            .with_headers(resp_headers)),
    }
}
