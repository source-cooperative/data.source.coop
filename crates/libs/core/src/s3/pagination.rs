//! S3 ListObjectsV2 pagination as a post-processing step.
//!
//! `object_store::list_with_delimiter()` always fetches all results.
//! This module applies `max-keys`, `continuation-token`, and `start-after`
//! filtering on the full result set to produce S3-compliant paginated responses.

use base64::Engine;

use crate::error::ProxyError;
use crate::s3::response::{ListCommonPrefix, ListContents};

const DEFAULT_MAX_KEYS: usize = 1000;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

pub struct PaginationParams {
    pub max_keys: usize,
    pub continuation_token: Option<String>,
    pub start_after: Option<String>,
}

pub struct PaginatedList {
    pub contents: Vec<ListContents>,
    pub common_prefixes: Vec<ListCommonPrefix>,
    pub is_truncated: bool,
    pub next_continuation_token: Option<String>,
}

/// Parse `max-keys`, `continuation-token`, and `start-after` from a query string.
pub fn parse_pagination_params(raw_query: Option<&str>) -> PaginationParams {
    let pairs = url::form_urlencoded::parse(raw_query.unwrap_or("").as_bytes());
    let find = |name| pairs.clone().find(|(k, _)| k == name).map(|(_, v)| v.to_string());

    PaginationParams {
        max_keys: find("max-keys")
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MAX_KEYS)
            .min(DEFAULT_MAX_KEYS),
        continuation_token: find("continuation-token"),
        start_after: find("start-after"),
    }
}

enum Entry {
    Object(ListContents),
    Prefix(ListCommonPrefix),
}

impl Entry {
    fn key(&self) -> &str {
        match self {
            Self::Object(c) => &c.key,
            Self::Prefix(p) => &p.prefix,
        }
    }
}

/// Apply pagination to a full list of objects and common prefixes.
pub fn paginate(
    contents: Vec<ListContents>,
    common_prefixes: Vec<ListCommonPrefix>,
    params: &PaginationParams,
) -> Result<PaginatedList, ProxyError> {
    // Decode continuation token (takes precedence over start-after per S3 spec)
    let decoded_token = params
        .continuation_token
        .as_deref()
        .map(|t| {
            B64.decode(t)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .ok_or_else(|| ProxyError::InvalidRequest("invalid continuation token".into()))
        })
        .transpose()?;

    let start_after = decoded_token.as_deref().or(params.start_after.as_deref());

    // Merge into a single sorted list
    let mut entries: Vec<Entry> = contents
        .into_iter()
        .map(Entry::Object)
        .chain(common_prefixes.into_iter().map(Entry::Prefix))
        .collect();
    entries.sort_by(|a, b| a.key().cmp(b.key()));

    // Filter by start-after, then take max_keys + 1 to detect truncation
    let mut page: Vec<Entry> = match start_after {
        Some(s) => entries.into_iter().filter(|e| e.key() > s).collect(),
        None => entries,
    };

    let is_truncated = page.len() > params.max_keys;
    page.truncate(params.max_keys);

    let next_continuation_token =
        if is_truncated { page.last().map(|e| B64.encode(e.key())) } else { None };

    // Split back into contents and common_prefixes
    let mut result_contents = Vec::new();
    let mut result_prefixes = Vec::new();
    for entry in page {
        match entry {
            Entry::Object(c) => result_contents.push(c),
            Entry::Prefix(p) => result_prefixes.push(p),
        }
    }

    Ok(PaginatedList {
        contents: result_contents,
        common_prefixes: result_prefixes,
        is_truncated,
        next_continuation_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_contents(keys: &[&str]) -> Vec<ListContents> {
        keys.iter()
            .map(|k| ListContents {
                key: k.to_string(),
                last_modified: "2024-01-01T00:00:00.000Z".to_string(),
                etag: "\"abc\"".to_string(),
                size: 100,
                storage_class: "STANDARD",
            })
            .collect()
    }

    fn make_prefixes(prefixes: &[&str]) -> Vec<ListCommonPrefix> {
        prefixes
            .iter()
            .map(|p| ListCommonPrefix { prefix: p.to_string() })
            .collect()
    }

    #[test]
    fn parse_defaults() {
        let p = parse_pagination_params(None);
        assert_eq!(p.max_keys, 1000);
        assert!(p.continuation_token.is_none());
        assert!(p.start_after.is_none());
    }

    #[test]
    fn parse_max_keys_clamped_to_1000() {
        assert_eq!(parse_pagination_params(Some("max-keys=5")).max_keys, 5);
        assert_eq!(parse_pagination_params(Some("max-keys=9999")).max_keys, 1000);
        assert_eq!(parse_pagination_params(Some("max-keys=abc")).max_keys, 1000);
    }

    #[test]
    fn parse_all_params() {
        let token = B64.encode("some-key");
        let q = format!("max-keys=2&continuation-token={}&start-after=aaa", token);
        let p = parse_pagination_params(Some(&q));
        assert_eq!(p.max_keys, 2);
        assert_eq!(p.continuation_token.as_deref(), Some(token.as_str()));
        assert_eq!(p.start_after.as_deref(), Some("aaa"));
    }

    #[test]
    fn no_truncation() {
        let r = paginate(make_contents(&["a", "b", "c"]), vec![], &PaginationParams {
            max_keys: 1000, continuation_token: None, start_after: None,
        }).unwrap();
        assert_eq!(r.contents.len(), 3);
        assert!(!r.is_truncated);
        assert!(r.next_continuation_token.is_none());
    }

    #[test]
    fn truncation_and_token() {
        let r = paginate(make_contents(&["a", "b", "c", "d", "e"]), vec![], &PaginationParams {
            max_keys: 2, continuation_token: None, start_after: None,
        }).unwrap();
        assert_eq!(r.contents.len(), 2);
        assert!(r.is_truncated);
        assert_eq!(r.contents[0].key, "a");
        assert_eq!(r.contents[1].key, "b");
        assert!(r.next_continuation_token.is_some());
    }

    #[test]
    fn continuation_token_round_trip() {
        let items = make_contents(&["a", "b", "c", "d", "e"]);
        let mk = |token| PaginationParams { max_keys: 2, continuation_token: token, start_after: None };

        let p1 = paginate(items.clone(), vec![], &mk(None)).unwrap();
        assert_eq!(p1.contents.iter().map(|c| &c.key).collect::<Vec<_>>(), &["a", "b"]);

        let p2 = paginate(items.clone(), vec![], &mk(p1.next_continuation_token)).unwrap();
        assert_eq!(p2.contents.iter().map(|c| &c.key).collect::<Vec<_>>(), &["c", "d"]);

        let p3 = paginate(items.clone(), vec![], &mk(p2.next_continuation_token)).unwrap();
        assert_eq!(p3.contents.iter().map(|c| &c.key).collect::<Vec<_>>(), &["e"]);
        assert!(!p3.is_truncated);
    }

    #[test]
    fn start_after() {
        let r = paginate(make_contents(&["a", "b", "c", "d"]), vec![], &PaginationParams {
            max_keys: 1000, continuation_token: None, start_after: Some("b".into()),
        }).unwrap();
        assert_eq!(r.contents.iter().map(|c| &c.key).collect::<Vec<_>>(), &["c", "d"]);
    }

    #[test]
    fn continuation_token_overrides_start_after() {
        let r = paginate(make_contents(&["a", "b", "c", "d", "e"]), vec![], &PaginationParams {
            max_keys: 1000,
            continuation_token: Some(B64.encode("c")),
            start_after: Some("a".into()),
        }).unwrap();
        assert_eq!(r.contents.iter().map(|c| &c.key).collect::<Vec<_>>(), &["d", "e"]);
    }

    #[test]
    fn interleaved_objects_and_prefixes() {
        let r = paginate(
            make_contents(&["a.txt", "c.txt"]),
            make_prefixes(&["b/", "d/"]),
            &PaginationParams { max_keys: 3, continuation_token: None, start_after: None },
        ).unwrap();
        assert_eq!(r.contents.len(), 2);
        assert_eq!(r.common_prefixes.len(), 1);
        assert_eq!(r.contents[0].key, "a.txt");
        assert_eq!(r.common_prefixes[0].prefix, "b/");
        assert_eq!(r.contents[1].key, "c.txt");
        assert!(r.is_truncated);
    }

    #[test]
    fn invalid_token_returns_error() {
        let r = paginate(make_contents(&["a"]), vec![], &PaginationParams {
            max_keys: 1000, continuation_token: Some("not-valid!!!".into()), start_after: None,
        });
        assert!(r.is_err());
    }

    #[test]
    fn max_keys_zero() {
        let r = paginate(make_contents(&["a", "b"]), vec![], &PaginationParams {
            max_keys: 0, continuation_token: None, start_after: None,
        }).unwrap();
        assert!(r.contents.is_empty());
        assert!(r.is_truncated);
    }

    #[test]
    fn empty_input() {
        let r = paginate(vec![], vec![], &PaginationParams {
            max_keys: 1000, continuation_token: None, start_after: None,
        }).unwrap();
        assert_eq!(r.contents.len(), 0);
        assert!(!r.is_truncated);
    }
}
