//! Stream abstraction for runtime-agnostic body handling.
//!
//! The key insight: the core proxy logic almost never needs to inspect or
//! transform the bytes flowing through it. For GET/PUT, the body is opaque —
//! it comes in from one side and goes out the other. This means our trait
//! can be minimal: we just need to know a body type exists and can be passed
//! around.
//!
//! Each runtime provides its own concrete type:
//! - Server runtime: `hyper::body::Incoming` / `http_body_util::Full<Bytes>`
//! - Worker runtime: a wrapper around JS `ReadableStream`
//!
//! The only time the core reads body bytes is for `CompleteMultipartUpload`
//! (parsing the XML manifest), which uses the `read_to_bytes` method.

use bytes::Bytes;
use std::future::Future;

use crate::maybe_send::MaybeSend;

/// Trait representing a streaming body type.
///
/// This is intentionally minimal. The core passes bodies through opaquely;
/// it never iterates over chunks except when it must parse a small request body.
pub trait BodyStream: Sized + MaybeSend + 'static {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Consume the body and collect all bytes.
    /// Used only for small bodies like XML manifests, never for large object data.
    fn read_to_bytes(self) -> impl Future<Output = Result<Bytes, Self::Error>> + MaybeSend;

    /// Create a body from raw bytes.
    fn from_bytes(bytes: Bytes) -> Self;

    /// Create an empty body.
    fn empty() -> Self;

    /// Content length, if known.
    fn content_length(&self) -> Option<u64>;
}
