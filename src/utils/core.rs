use actix_web::{
    body::{BodySize, MessageBody},
    web, Error as ActixError,
};
use bytes::Bytes;
use futures::Stream;
use pin_project_lite::pin_project;
use rusoto_core::ByteStream;
use std::collections::HashMap;
use std::io::Read;
use std::pin::Pin;
use std::task::{Context, Poll};
use url::form_urlencoded;

pin_project! {
    pub struct StreamingResponse<S> {
        #[pin]
        inner: S,
        size: u64,
    }
}

pub fn get_query_params(query: &str) -> HashMap<String, String> {
    form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

impl<S> StreamingResponse<S> {
    pub fn new(inner: S, size: u64) -> Self {
        Self { inner, size }
    }
}

impl<S> MessageBody for StreamingResponse<S>
where
    S: Stream,
    S::Item: Into<Result<web::Bytes, ActixError>>,
{
    type Error = ActixError;

    fn size(&self) -> BodySize {
        BodySize::Sized(self.size)
    }

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<web::Bytes, Self::Error>>> {
        let this = self.project();
        match this.inner.poll_next(cx) {
            Poll::Ready(Some(item)) => Poll::Ready(Some(item.into())),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub struct FakeBody {
    pub size: usize,
}

impl MessageBody for FakeBody {
    type Error = actix_web::Error;
    fn size(&self) -> BodySize {
        BodySize::Sized(self.size as u64)
    }

    fn poll_next(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<web::Bytes, actix_web::Error>>> {
        Poll::Ready(None)
    }
}

pub fn replace_first(original: String, from: String, to: String) -> String {
    match original.find(&from) {
        Some(start_index) => {
            let mut result = String::with_capacity(original.len());
            result.push_str(&original[..start_index]);
            result.push_str(&to);
            result.push_str(&original[start_index + from.len()..]);
            result
        }
        None => original,
    }
}

/// Splits a string at the first forward slash ('/') character.
///
/// This function takes a string as input and returns a tuple of two strings.
/// The first string in the tuple contains the part of the input before the
/// first slash, and the second string contains the part after the first slash.
///
/// # Arguments
///
/// * `input` - A String that may or may not contain a forward slash.
///
/// # Returns
///
/// A tuple `(String, String)` where:
/// - The first element is the substring before the first slash.
/// - The second element is the substring after the first slash.
///
/// If there is no slash in the input string, the function returns the entire
/// input as the first element of the tuple and an empty string as the second element.
///
/// # Examples
///
/// ```
/// let (before, after) = split_at_first_slash("path/to/file".to_string());
/// assert_eq!(before, "path");
/// assert_eq!(after, "to/file");
///
/// let (before, after) = split_at_first_slash("no_slash".to_string());
/// assert_eq!(before, "no_slash");
/// assert_eq!(after, "");
/// ```
pub fn split_at_first_slash(input: &str) -> (&str, &str) {
    match input.find('/') {
        Some(index) => {
            let (before, after) = input.split_at(index);
            (before, &after[1..])
        }
        None => (input, ""),
    }
}

pin_project! {
    pub struct GenericByteStream<T> {
        #[pin]
        inner: T,
    }
}

impl<T> GenericByteStream<T> {
    pub fn new(inner: T) -> Self {
        GenericByteStream { inner }
    }
}

impl<T: Stream<Item = Result<Bytes, Box<dyn std::error::Error + Send + Sync>>> + Unpin> Stream
    for GenericByteStream<T>
{
    type Item = Result<Bytes, Box<dyn std::error::Error + Send + Sync>>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.project().inner.poll_next(cx)
    }
}

// Implement From for Rusoto ByteStream
impl From<rusoto_core::ByteStream> for GenericByteStream<rusoto_core::ByteStream> {
    fn from(stream: rusoto_core::ByteStream) -> Self {
        GenericByteStream::new(stream)
    }
}

// Implement From for reqwest::Response bytes_stream
impl From<reqwest::Response>
    for GenericByteStream<Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>>
{
    fn from(response: reqwest::Response) -> Self {
        let stream = response.bytes_stream();
        GenericByteStream::new(Box::pin(stream))
    }
}

use reqwest::Error as ReqwestError;
type BoxedReqwestStream = Pin<Box<dyn Stream<Item = Result<Bytes, ReqwestError>> + Send>>;
