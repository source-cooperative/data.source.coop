//! Worker body type implementing `BodyStream`.
//!
//! The key optimization: response bodies from the backend Fetch API are
//! `ReadableStream` objects in JS. Rather than reading them into Rust memory,
//! we pass them through opaquely. The stream only touches Rust when the core
//! needs to parse a small body (e.g., CompleteMultipartUpload XML manifest).

use bytes::Bytes;
use js_sys::Uint8Array;
use s3_proxy_core::stream::BodyStream;
use wasm_bindgen_futures::JsFuture;

/// Body type for the Cloudflare Workers runtime.
///
/// Most request/response bodies flow through as opaque JS `ReadableStream`
/// objects, never touching Rust memory. The `Bytes` variant is used only
/// for small bodies constructed in Rust (error responses, XML manifests).
pub enum WorkerBody {
    /// Raw bytes (small bodies constructed in Rust).
    Bytes(Bytes),
    /// A JS ReadableStream passed through opaquely.
    Stream(web_sys::ReadableStream),
    /// No body.
    Empty,
}

#[derive(Debug, thiserror::Error)]
#[error("worker body error: {0}")]
pub struct WorkerBodyError(pub String);

impl BodyStream for WorkerBody {
    type Error = WorkerBodyError;

    async fn read_to_bytes(self) -> Result<Bytes, Self::Error> {
        match self {
            WorkerBody::Bytes(b) => Ok(b),
            WorkerBody::Empty => Ok(Bytes::new()),
            WorkerBody::Stream(stream) => {
                // Consume the ReadableStream into bytes.
                // This is only called for small bodies (XML manifests), never for
                // large object data.
                read_stream_to_bytes(stream).await
            }
        }
    }

    fn from_bytes(bytes: Bytes) -> Self {
        if bytes.is_empty() {
            WorkerBody::Empty
        } else {
            WorkerBody::Bytes(bytes)
        }
    }

    fn empty() -> Self {
        WorkerBody::Empty
    }

    fn content_length(&self) -> Option<u64> {
        match self {
            WorkerBody::Bytes(b) => Some(b.len() as u64),
            WorkerBody::Empty => Some(0),
            // Stream length is unknown — the backend will set Content-Length
            // in the response headers if applicable.
            WorkerBody::Stream(_) => None,
        }
    }
}

impl WorkerBody {
    /// Convert to a JsValue for use as a Fetch API request body.
    /// Returns `None` for empty bodies (Fetch API interprets absent body as no body).
    pub fn into_js_body(self) -> Option<wasm_bindgen::JsValue> {
        match self {
            WorkerBody::Empty => None,
            WorkerBody::Bytes(b) => {
                let uint8 = Uint8Array::from(b.as_ref());
                Some(uint8.into())
            }
            WorkerBody::Stream(stream) => Some(stream.into()),
        }
    }

    /// Create a `WorkerBody` from a `web_sys::Response` by extracting its
    /// ReadableStream body without consuming it into bytes.
    pub fn from_ws_response(response: &web_sys::Response) -> Self {
        match response.body() {
            Some(stream) => WorkerBody::Stream(stream),
            None => WorkerBody::Empty,
        }
    }

    /// Create a `WorkerBody` from a `web_sys::Request` by extracting its
    /// ReadableStream body.
    pub fn from_ws_request(request: &web_sys::Request) -> Self {
        match request.body() {
            Some(stream) => WorkerBody::Stream(stream),
            None => WorkerBody::Empty,
        }
    }
}

/// Read a JS ReadableStream to completion, collecting all chunks into `Bytes`.
///
/// Uses the JS `Response` constructor trick: `new Response(stream).arrayBuffer()`
/// which is the most efficient way to consume a stream in Workers.
async fn read_stream_to_bytes(stream: web_sys::ReadableStream) -> Result<Bytes, WorkerBodyError> {
    // Create a Response from the stream, then read its arrayBuffer
    let response = web_sys::Response::new_with_opt_readable_stream(Some(&stream))
        .map_err(|e| WorkerBodyError(format!("failed to wrap stream in Response: {:?}", e)))?;

    let array_buffer_promise = response
        .array_buffer()
        .map_err(|e| WorkerBodyError(format!("failed to get arrayBuffer: {:?}", e)))?;

    let array_buffer = JsFuture::from(array_buffer_promise)
        .await
        .map_err(|e| WorkerBodyError(format!("failed to read arrayBuffer: {:?}", e)))?;

    let uint8 = Uint8Array::new(&array_buffer);
    Ok(Bytes::from(uint8.to_vec()))
}
