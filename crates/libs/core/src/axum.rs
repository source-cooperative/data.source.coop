//! Axum response helpers shared across runtimes.
//!
//! Gated behind the `axum` feature flag so the core crate remains usable
//! without pulling in axum.

use ::axum::body::Body;
use ::axum::response::Response;

use crate::proxy::ProxyResult;
use crate::response_body::ProxyResponseBody;

/// Convert a [`ProxyResult`] to an axum [`Response`].
pub fn build_proxy_response(result: ProxyResult) -> Response {
    let body = match result.body {
        ProxyResponseBody::Bytes(b) => Body::from(b),
        ProxyResponseBody::Empty => Body::empty(),
    };

    let mut builder = Response::builder().status(result.status);
    for (key, value) in result.headers.iter() {
        builder = builder.header(key, value);
    }

    builder.body(body).unwrap()
}

/// Build a plain-text error response.
pub fn error_response(status: u16, message: &str) -> Response {
    Response::builder()
        .status(status)
        .body(Body::from(message.to_string()))
        .unwrap()
}
