//! Server-side body type implementing `BodyStream`.

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Empty};
use s3_proxy_core::stream::BodyStream;

/// A body type for the server runtime.
///
/// Wraps either an incoming request body or a constructed response body.
/// Uses `http_body_util` types which integrate natively with Hyper.
pub enum ServerBody {
    /// A body constructed from known bytes.
    Full(Full<Bytes>),
    /// An empty body.
    Empty(Empty<Bytes>),
    /// A streaming body from reqwest (for backend responses).
    Streaming(reqwest::Response),
}

/// Error type for server body operations.
#[derive(Debug, thiserror::Error)]
pub enum ServerBodyError {
    #[error("hyper error: {0}")]
    Hyper(String),
    #[error("reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
}

impl BodyStream for ServerBody {
    type Error = ServerBodyError;

    async fn read_to_bytes(self) -> Result<Bytes, Self::Error> {
        match self {
            ServerBody::Full(full) => {
                let collected = full.collect().await.map_err(|e| ServerBodyError::Hyper(e.to_string()))?;
                Ok(collected.to_bytes())
            }
            ServerBody::Empty(_) => Ok(Bytes::new()),
            ServerBody::Streaming(resp) => {
                resp.bytes().await.map_err(ServerBodyError::Reqwest)
            }
        }
    }

    fn from_bytes(bytes: Bytes) -> Self {
        ServerBody::Full(Full::new(bytes))
    }

    fn empty() -> Self {
        ServerBody::Empty(Empty::new())
    }

    fn content_length(&self) -> Option<u64> {
        match self {
            ServerBody::Full(f) => {
                use hyper::body::Body;
                f.size_hint().exact()
            }
            ServerBody::Empty(_) => Some(0),
            ServerBody::Streaming(resp) => resp.content_length(),
        }
    }
}
