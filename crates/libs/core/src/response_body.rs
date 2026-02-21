//! Response body type for the proxy.
//!
//! [`ProxyResponseBody`] replaces the old generic body type parameter.
//! All runtimes convert this to their native response type at the edge.

use bytes::Bytes;
use futures::stream::BoxStream;

/// The body of a proxy response.
///
/// This is no longer generic — all runtimes work with this concrete type
/// and convert to their native response format.
pub enum ProxyResponseBody {
    /// Streaming response from `object_store` GET.
    /// Bytes arrive lazily in chunks.
    Stream(BoxStream<'static, Result<Bytes, object_store::Error>>),
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
