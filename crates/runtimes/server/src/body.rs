//! Response body conversion for the server runtime.
//!
//! Converts [`ProxyResponseBody`] to a streaming hyper response body.

use bytes::Bytes;
use futures::{Stream, TryStreamExt};
use http::Response;
use http_body_util::{Either, Empty, Full, StreamBody};
use hyper::body::Frame;
use s3_proxy_core::proxy::ProxyResult;
use s3_proxy_core::response_body::ProxyResponseBody;
use std::pin::Pin;

/// The server's native body type from `send_streaming`.
type NativeStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

/// A boxed streaming body type that erases concrete stream types.
type BoxedStreamBody = StreamBody<
    std::pin::Pin<
        Box<dyn futures::Stream<Item = Result<Frame<Bytes>, std::io::Error>> + Send>,
    >,
>;

/// The server response body type: either a stream, fixed bytes, or empty.
pub type ServerResponseBody = Either<BoxedStreamBody, Either<Full<Bytes>, Empty<Bytes>>>;

/// Convert a `ProxyResult` to a hyper `Response` with a streaming body.
pub fn build_hyper_response(
    result: ProxyResult<NativeStream>,
) -> Result<Response<ServerResponseBody>, Box<dyn std::error::Error + Send + Sync>> {
    let mut builder = Response::builder().status(result.status);

    for (key, value) in result.headers.iter() {
        builder = builder.header(key, value);
    }

    let body = match result.body {
        ProxyResponseBody::Native(stream) => {
            let framed = stream
                .map_ok(Frame::data)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()));
            Either::Left(StreamBody::new(Box::pin(framed) as std::pin::Pin<Box<dyn futures::Stream<Item = Result<Frame<Bytes>, std::io::Error>> + Send>>))
        }
        ProxyResponseBody::Stream(stream) => {
            let framed = stream
                .map_ok(Frame::data)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()));
            Either::Left(StreamBody::new(Box::pin(framed) as std::pin::Pin<Box<dyn futures::Stream<Item = Result<Frame<Bytes>, std::io::Error>> + Send>>))
        }
        ProxyResponseBody::Bytes(b) => Either::Right(Either::Left(Full::new(b))),
        ProxyResponseBody::Empty => Either::Right(Either::Right(Empty::new())),
    };

    Ok(builder.body(body)?)
}
