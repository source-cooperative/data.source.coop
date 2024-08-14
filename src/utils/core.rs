use actix_web::{
    body::{BodySize, MessageBody},
    web, Error as ActixError,
};
use futures::Stream;
use pin_project_lite::pin_project;
use std::pin::Pin;
use std::task::{Context, Poll};
use url::Url;

pin_project! {
    pub struct StreamingResponse<S> {
        #[pin]
        inner: S,
        size: u64,
    }
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

pub fn parse_azure_blob_url(url_str: String) -> Result<(String, String, String), String> {
    let url = Url::parse(&url_str).map_err(|e| format!("Invalid URL: {}", e))?;

    if url.scheme() != "https" {
        return Err("URL must use HTTPS scheme".to_string());
    }

    let host = url.host_str().ok_or("Missing host in URL")?;
    let host_parts: Vec<&str> = host.split('.').collect();

    if host_parts.len() < 5
        || host_parts[1] != "blob"
        || host_parts[2] != "core"
        || host_parts[3] != "windows"
        || host_parts[4] != "net"
    {
        return Err("Invalid Azure Blob Storage URL format".to_string());
    }

    let account_name = host_parts[0].to_string();

    let path_segments: Vec<&str> = url.path_segments().ok_or("No path in URL")?.collect();
    if path_segments.is_empty() {
        return Err("Missing container name in URL".to_string());
    }

    let container_name = path_segments[0].to_string();
    let prefix = path_segments[1..].join("/");

    Ok((account_name, container_name, prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_azure_blob_url() {
        let url = "https://radiantearth.blob.core.windows.net/mlhub/repositories/radiantearth/landcovernet".to_string();
        let result = parse_azure_blob_url(url).unwrap();
        assert_eq!(result.0, "radiantearth");
        assert_eq!(result.1, "mlhub");
        assert_eq!(result.2, "repositories/radiantearth/landcovernet");
    }

    #[test]
    fn test_invalid_url() {
        let url = "http://invalid-url.com".to_string();
        assert!(parse_azure_blob_url(url).is_err());
    }
}
