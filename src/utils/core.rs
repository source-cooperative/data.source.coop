use actix_web::{
    body::{BodySize, MessageBody},
    web, Error as ActixError,
};
use futures::Stream;
use pin_project_lite::pin_project;
use std::task::{Context, Poll};
use std::{pin::Pin, str::FromStr};

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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RangeRequest {
    /// The start byte offset
    pub start: u64,
    /// The end byte offset (inclusive), or `None` for open-ended ranges (e.g. "bytes=100-")
    pub end: Option<u64>,
}

impl From<RangeRequest> for String {
    fn from(r: RangeRequest) -> Self {
        match r.end {
            Some(end) => format!("bytes={}-{}", r.start, end),
            None => format!("bytes={}-", r.start),
        }
    }
}

impl FromStr for RangeRequest {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes_range = s.strip_prefix("bytes=").ok_or(())?;
        let (start_str, end_str) = bytes_range.split_once('-').ok_or(())?;
        let start = start_str.parse::<u64>().map_err(|_| ())?;

        let end = if end_str.is_empty() {
            None
        } else {
            let end = end_str.parse::<u64>().map_err(|_| ())?;
            if start > end {
                return Err(());
            }
            Some(end)
        };

        Ok(RangeRequest { start, end })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_range_full_range() {
        let result: RangeRequest = "bytes=0-1023".parse().unwrap();
        assert_eq!(
            result,
            RangeRequest {
                start: 0,
                end: Some(1023),
            }
        );
    }

    #[test]
    fn test_parse_range_open_ended() {
        let result = "bytes=100-".parse::<RangeRequest>().unwrap();
        assert_eq!(
            result,
            RangeRequest {
                start: 100,
                end: None,
            }
        );
    }

    #[test]
    fn test_parse_range_open_ended_no_total_size() {
        // Without with_total_size, end remains None
        let result: RangeRequest = "bytes=100-".parse().unwrap();
        assert_eq!(result.end, None);
    }

    #[test]
    fn test_parse_range_missing_prefix() {
        assert!("invalid=0-100".parse::<RangeRequest>().is_err());
    }

    #[test]
    fn test_parse_range_non_numeric_start() {
        assert!("bytes=abc-100".parse::<RangeRequest>().is_err());
    }

    #[test]
    fn test_parse_range_non_numeric_end() {
        assert!("bytes=0-abc".parse::<RangeRequest>().is_err());
    }

    #[test]
    fn test_parse_range_start_greater_than_end() {
        assert!("bytes=500-100".parse::<RangeRequest>().is_err());
    }

    #[test]
    fn test_parse_range_start_beyond_total_size() {
        // Parsing succeeds; validation against total_size is the caller's responsibility
        let result: RangeRequest = "bytes=1000-1023".parse().unwrap();
        assert_eq!(result.start, 1000);
        assert_eq!(result.end, Some(1023));
    }

    #[test]
    fn test_parse_range_single_byte() {
        let result: RangeRequest = "bytes=0-0".parse().unwrap();
        assert_eq!(
            result,
            RangeRequest {
                start: 0,
                end: Some(0),
            }
        );
    }

    #[test]
    fn test_parse_range_large_file() {
        let rr: RangeRequest = "bytes=0-1023".parse().unwrap();
        assert_eq!(rr.start, 0);
        assert_eq!(rr.end, Some(1023));
        assert_eq!(rr.end.unwrap() - rr.start + 1, 1024);
    }

    #[test]
    fn test_parse_range_no_hyphen() {
        assert!("bytes=100".parse::<RangeRequest>().is_err());
    }

    #[test]
    fn test_parse_range_content_length_calculation() {
        let result: RangeRequest = "bytes=0-1023".parse().unwrap();
        assert_eq!(result.end.unwrap() - result.start + 1, 1024);
    }

    #[test]
    fn test_parse_range_content_range_format() {
        let result: RangeRequest = "bytes=0-1023".parse().unwrap();
        let content_range = format!(
            "bytes {}-{}/{}",
            result.start,
            result.end.unwrap(),
            3515053862u64
        );
        assert_eq!(content_range, "bytes 0-1023/3515053862");
    }
}
