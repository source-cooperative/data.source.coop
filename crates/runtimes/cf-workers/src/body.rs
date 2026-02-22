//! Response body conversion for the Cloudflare Workers runtime.
//!
//! Converts [`ProxyResponseBody`] to `worker::Response`.

use futures::StreamExt;
use js_sys::Uint8Array;
use s3_proxy_core::proxy::ProxyResult;
use s3_proxy_core::response_body::ProxyResponseBody;
use wasm_bindgen_futures::spawn_local;
use worker::{Headers, Response};

/// Build a `worker::Response` from a `ProxyResult`.
///
/// Stream bodies are bridged to JS `ReadableStream` via a `TransformStream`:
/// a spawn_local task reads Rust stream chunks and writes them to the
/// writable side; the readable side is used for the Response.
pub fn build_worker_response(result: ProxyResult) -> Result<Response, worker::Error> {
    let resp_headers = Headers::new();
    for (key, value) in result.headers.iter() {
        if let Ok(v) = value.to_str() {
            let _ = resp_headers.set(key.as_str(), v);
        }
    }

    match result.body {
        // TODO: This seems like it violates the need to keep streams in the JS form
        ProxyResponseBody::Stream(stream) => {
            // Bridge Rust Stream<Bytes> -> JS ReadableStream via TransformStream
            let transform = web_sys::TransformStream::new()
                .map_err(|e| worker::Error::RustError(format!("TransformStream error: {:?}", e)))?;

            let writable = transform.writable();
            let readable = transform.readable();

            // Spawn a task to pump chunks from the Rust stream into the JS writable side
            spawn_local(async move {
                let writer = writable.get_writer().unwrap();
                let mut stream = stream;

                while let Some(chunk_result) = stream.next().await {
                    match chunk_result {
                        Ok(bytes) => {
                            let uint8 = Uint8Array::from(bytes.as_ref());
                            if let Err(_) = wasm_bindgen_futures::JsFuture::from(
                                writer.write_with_chunk(&uint8.into()),
                            )
                            .await
                            {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let _ = wasm_bindgen_futures::JsFuture::from(writer.close()).await;
            });

            // Build the response from the readable side
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
                web_sys::Response::new_with_opt_readable_stream_and_init(Some(&readable), &init)
                    .map_err(|e| {
                        worker::Error::RustError(format!("failed to build response: {:?}", e))
                    })?;

            Ok(ws_response.into())
        }
        ProxyResponseBody::Bytes(b) => Ok(Response::from_bytes(b.to_vec())?
            .with_status(result.status)
            .with_headers(resp_headers)),
        ProxyResponseBody::Empty => Ok(Response::from_bytes(vec![])?
            .with_status(result.status)
            .with_headers(resp_headers)),
    }
}
