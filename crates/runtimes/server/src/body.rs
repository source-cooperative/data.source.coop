//! Response body conversion for the server runtime.
//!
//! Converts [`ProxyResult`] to a hyper response body. Streaming responses
//! (from Forward requests) are handled directly in `server.rs`.

use bytes::Bytes;
use futures::Stream;
use http::Response;
use http_body_util::{Either, Empty, Full, StreamBody};
use hyper::body::Frame;
use s3_proxy_core::proxy::ProxyResult;
use s3_proxy_core::response_body::ProxyResponseBody;
use std::pin::Pin;

/// A boxed streaming body type that erases concrete stream types.
type BoxedStreamBody =
    StreamBody<Pin<Box<dyn Stream<Item = Result<Frame<Bytes>, std::io::Error>> + Send>>>;

/// The server response body type: either a stream (Forward) or fixed bytes/empty (Response).
pub type ServerResponseBody = Either<BoxedStreamBody, Either<Full<Bytes>, Empty<Bytes>>>;

/// Convert a `ProxyResult` to a hyper `Response`.
///
/// Only handles `Bytes` and `Empty` bodies (LIST, errors, multipart responses).
/// Streaming Forward responses are built directly in `server.rs`.
pub fn build_hyper_response(
    result: ProxyResult,
) -> Result<Response<ServerResponseBody>, Box<dyn std::error::Error + Send + Sync>> {
    let mut builder = Response::builder().status(result.status);

    for (key, value) in result.headers.iter() {
        builder = builder.header(key, value);
    }

    let body = match result.body {
        ProxyResponseBody::Bytes(b) => Either::Right(Either::Left(Full::new(b))),
        ProxyResponseBody::Empty => Either::Right(Either::Right(Empty::new())),
    };

    Ok(builder.body(body)?)
}
