#[path = "../src/pagination.rs"]
mod pagination;

use multistore::api::list::parse_list_query_params;
use pagination::paginate_prefixes;

// ── parse_list_query_params ────────────────────────────────────────

#[test]
fn parse_defaults_when_no_query() {
    let params = parse_list_query_params(None);
    assert_eq!(params.max_keys, 1000);
    assert!(params.continuation_token.is_none());
    assert!(params.start_after.is_none());
}

#[test]
fn parse_max_keys() {
    let params = parse_list_query_params(Some("list-type=2&max-keys=5"));
    assert_eq!(params.max_keys, 5);
}

#[test]
fn parse_max_keys_capped_at_1000() {
    let params = parse_list_query_params(Some("max-keys=9999"));
    assert_eq!(params.max_keys, 1000);
}

#[test]
fn parse_continuation_token() {
    let params = parse_list_query_params(Some("continuation-token=abc%2Fdef"));
    assert_eq!(params.continuation_token.as_deref(), Some("abc/def"));
}

#[test]
fn parse_start_after() {
    let params = parse_list_query_params(Some("start-after=foo/"));
    assert_eq!(params.start_after.as_deref(), Some("foo/"));
}

// ── paginate_prefixes ──────────────────────────────────────────────

fn prefixes(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

#[test]
fn no_params_returns_all() {
    let params = parse_list_query_params(None);
    let result = paginate_prefixes(prefixes(&["c/", "a/", "b/"]), &params);
    assert_eq!(result.prefixes, vec!["a/", "b/", "c/"]);
    assert!(!result.is_truncated);
    assert!(result.next_continuation_token.is_none());
}

#[test]
fn max_keys_truncates() {
    let params = parse_list_query_params(Some("max-keys=2"));
    let result = paginate_prefixes(prefixes(&["e/", "d/", "c/", "b/", "a/"]), &params);
    assert_eq!(result.prefixes, vec!["a/", "b/"]);
    assert!(result.is_truncated);
    assert_eq!(result.next_continuation_token.as_deref(), Some("b/"));
}

#[test]
fn continuation_token_skips_before() {
    let params = parse_list_query_params(Some("continuation-token=b/"));
    let result = paginate_prefixes(prefixes(&["a/", "b/", "c/", "d/"]), &params);
    assert_eq!(result.prefixes, vec!["c/", "d/"]);
    assert!(!result.is_truncated);
}

#[test]
fn start_after_skips_before() {
    let params = parse_list_query_params(Some("start-after=b/"));
    let result = paginate_prefixes(prefixes(&["a/", "b/", "c/", "d/"]), &params);
    assert_eq!(result.prefixes, vec!["c/", "d/"]);
    assert!(!result.is_truncated);
}

#[test]
fn continuation_token_takes_precedence_over_start_after() {
    let params = parse_list_query_params(Some("start-after=a/&continuation-token=c/"));
    let result = paginate_prefixes(prefixes(&["a/", "b/", "c/", "d/", "e/"]), &params);
    // continuation-token=c/ should win, skipping a/, b/, c/
    assert_eq!(result.prefixes, vec!["d/", "e/"]);
}

#[test]
fn pagination_with_max_keys_and_continuation() {
    let params = parse_list_query_params(Some("max-keys=2&continuation-token=b/"));
    let result = paginate_prefixes(prefixes(&["a/", "b/", "c/", "d/", "e/"]), &params);
    assert_eq!(result.prefixes, vec!["c/", "d/"]);
    assert!(result.is_truncated);
    assert_eq!(result.next_continuation_token.as_deref(), Some("d/"));
}

#[test]
fn empty_list_returns_empty() {
    let params = parse_list_query_params(Some("max-keys=10"));
    let result = paginate_prefixes(vec![], &params);
    assert!(result.prefixes.is_empty());
    assert!(!result.is_truncated);
    assert!(result.next_continuation_token.is_none());
}

#[test]
fn max_keys_zero_returns_empty_but_truncated() {
    let params = parse_list_query_params(Some("max-keys=0"));
    let result = paginate_prefixes(prefixes(&["a/", "b/"]), &params);
    assert!(result.prefixes.is_empty());
    assert!(result.is_truncated);
}
