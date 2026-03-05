use actix_web::{
    body::{BodySize, MessageBody},
    http::header::RANGE,
    web, Error as ActixError, HttpRequest,
};
use futures::Stream;
use pin_project_lite::pin_project;
use std::pin::Pin;
use std::task::{Context, Poll};

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

/// Parsed range request information.
#[derive(Debug, Clone, PartialEq)]
pub struct RangeRequest {
    /// The start byte offset
    pub start: u64,
    /// The end byte offset (inclusive)
    pub end: u64,
    /// The range value to forward upstream (e.g. "bytes=0-1023")
    pub header_value: String,
}

/// Parses an HTTP Range header value into a `RangeRequest`.
///
/// Supports the format `bytes=<start>-<end>` where `<end>` is optional.
/// When `<end>` is omitted and `total_size` is provided, the end defaults
/// to `total_size - 1`. When `<end>` is omitted and `total_size` is `None`,
/// the raw header value is preserved for upstream forwarding.
///
/// Returns `None` if the header is missing, malformed, or has an invalid format.
pub fn parse_range_header(req: &HttpRequest, total_size: Option<u64>) -> Option<RangeRequest> {
    let range_str = req.headers().get(RANGE)?.to_str().ok()?;
    parse_range_str(range_str, total_size)
}

/// Parses a raw Range header string (e.g. "bytes=0-1023") into a `RangeRequest`.
pub fn parse_range_str(range_str: &str, total_size: Option<u64>) -> Option<RangeRequest> {
    let bytes_range = range_str.strip_prefix("bytes=")?;
    let (start_str, end_str) = bytes_range.split_once('-')?;
    let start = start_str.parse::<u64>().ok()?;

    let end = if end_str.is_empty() {
        total_size.map(|s| s - 1)?
    } else {
        end_str.parse::<u64>().ok()?
    };

    if start > end {
        return None;
    }

    if let Some(size) = total_size {
        if start >= size {
            return None;
        }
    }

    Some(RangeRequest {
        start,
        end,
        header_value: format!("bytes={}-{}", start, end_str),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_range_full_range() {
        let result = parse_range_str("bytes=0-1023", Some(3515053862));
        assert_eq!(
            result,
            Some(RangeRequest {
                start: 0,
                end: 1023,
                header_value: "bytes=0-1023".to_string(),
            })
        );
    }

    #[test]
    fn test_parse_range_open_ended() {
        let result = parse_range_str("bytes=100-", Some(1000));
        assert_eq!(
            result,
            Some(RangeRequest {
                start: 100,
                end: 999,
                header_value: "bytes=100-".to_string(),
            })
        );
    }

    #[test]
    fn test_parse_range_open_ended_no_total_size() {
        // Without total_size, open-ended range cannot be resolved
        let result = parse_range_str("bytes=100-", None);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_missing_prefix() {
        let result = parse_range_str("invalid=0-100", Some(1000));
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_non_numeric_start() {
        let result = parse_range_str("bytes=abc-100", Some(1000));
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_non_numeric_end() {
        let result = parse_range_str("bytes=0-abc", Some(1000));
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_start_greater_than_end() {
        let result = parse_range_str("bytes=500-100", Some(1000));
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_start_beyond_total_size() {
        let result = parse_range_str("bytes=1000-1023", Some(1000));
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_single_byte() {
        let result = parse_range_str("bytes=0-0", Some(1000));
        assert_eq!(
            result,
            Some(RangeRequest {
                start: 0,
                end: 0,
                header_value: "bytes=0-0".to_string(),
            })
        );
    }

    #[test]
    fn test_parse_range_large_file() {
        let total = 3515053862u64;
        let result = parse_range_str("bytes=0-1023", Some(total));
        let rr = result.unwrap();
        assert_eq!(rr.start, 0);
        assert_eq!(rr.end, 1023);
        assert_eq!(rr.end - rr.start + 1, 1024);
    }

    #[test]
    fn test_parse_range_preserves_open_end_in_header_value() {
        // The header_value should preserve the original format for upstream forwarding
        let result = parse_range_str("bytes=100-", Some(1000));
        let rr = result.unwrap();
        assert_eq!(rr.header_value, "bytes=100-");
    }

    #[test]
    fn test_parse_range_no_hyphen() {
        let result = parse_range_str("bytes=100", Some(1000));
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_content_length_calculation() {
        let result = parse_range_str("bytes=0-1023", Some(5000)).unwrap();
        assert_eq!(result.end - result.start + 1, 1024);
    }

    #[test]
    fn test_parse_range_content_range_format() {
        let result = parse_range_str("bytes=0-1023", Some(3515053862)).unwrap();
        let content_range = format!("bytes {}-{}/{}", result.start, result.end, 3515053862u64);
        assert_eq!(content_range, "bytes 0-1023/3515053862");
    }
}
