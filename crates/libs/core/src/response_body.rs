//! Response body type for the proxy.
//!
//! [`ProxyResponseBody`] is generic over a native body type `N` so that each
//! runtime can carry its platform-native streaming body through the handler
//! without intermediate conversion (e.g. JS ReadableStream on CF Workers).

use bytes::Bytes;
use futures::stream::BoxStream;

/// The body of a proxy response.
///
/// Generic over `N`, the runtime's native streaming body type.
/// The default `N = ()` is used for responses that don't carry a native body
/// (errors, list XML, HEAD, etc.).
pub enum ProxyResponseBody<N = ()> {
    /// Streaming response from `object_store` GET (non-S3 backends).
    /// Bytes arrive lazily in chunks.
    Stream(BoxStream<'static, Result<Bytes, object_store::Error>>),
    /// Fixed bytes (error XML, list XML, multipart XML responses, etc.).
    Bytes(Bytes),
    /// Empty body (HEAD responses, etc.).
    Empty,
    /// Runtime-native streaming body, bypassing Rust stream intermediaries.
    /// On CF Workers this is `Option<web_sys::ReadableStream>`;
    /// on the server runtime it's a reqwest byte stream.
    Native(N),
}

impl<N> ProxyResponseBody<N> {
    /// Create a response body from raw bytes.
    pub fn from_bytes(bytes: Bytes) -> Self {
        if bytes.is_empty() {
            Self::Empty
        } else {
            Self::Bytes(bytes)
        }
    }

    /// Create an empty response body.
    pub fn empty() -> Self {
        Self::Empty
    }
}
