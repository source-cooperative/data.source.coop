//! Response body type for the proxy.
//!
//! [`ProxyResponseBody`] carries non-streaming response data. Streaming
//! responses (GET, PUT) are handled by the runtime via [`ForwardRequest`]
//! presigned URLs — the handler never touches those bytes.

use bytes::Bytes;

/// The body of a proxy response.
///
/// Only used for responses the handler constructs directly (errors, LIST XML,
/// multipart XML, HEAD metadata). Streaming GET/PUT bodies bypass this type
/// entirely via the `Forward` action.
pub enum ProxyResponseBody {
    /// Fixed bytes (error XML, list XML, multipart XML responses, etc.).
    Bytes(Bytes),
    /// Empty body (HEAD responses, etc.).
    Empty,
}

impl ProxyResponseBody {
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
