// Include routing.rs directly so we can test on native targets without
// linking the full lib crate (which depends on wasm-only crates).
#[path = "../src/routing.rs"]
mod routing;

use multistore_path_mapping::PathMapping;
use routing::{
    classify_request, extract_query_param, is_list_request, rewrite_prefix_in_query, RequestClass,
};

fn mapping() -> PathMapping {
    PathMapping {
        bucket_segments: 2,
        bucket_separator: "--".to_string(),
        display_bucket_segments: 1,
    }
}

// ── classify_request ────────────────────────────────────────────────

#[test]
fn root_path_returns_index() {
    assert_eq!(classify_request(&mapping(), "/", None), RequestClass::Index);
}

#[test]
fn empty_path_returns_index() {
    assert_eq!(classify_request(&mapping(), "", None), RequestClass::Index);
}

#[test]
fn object_request_two_segments_plus_key() {
    assert_eq!(
        classify_request(&mapping(), "/cholmes/admin-boundaries/file.parquet", None),
        RequestClass::ProxyRequest {
            rewritten_path: "/cholmes--admin-boundaries/file.parquet".to_string(),
            query: None,
        }
    );
}

#[test]
fn object_request_nested_key() {
    assert_eq!(
        classify_request(
            &mapping(),
            "/cholmes/admin-boundaries/dir/sub/file.parquet",
            None
        ),
        RequestClass::ProxyRequest {
            rewritten_path: "/cholmes--admin-boundaries/dir/sub/file.parquet".to_string(),
            query: None,
        }
    );
}

#[test]
fn product_list_via_path_segment() {
    assert_eq!(
        classify_request(&mapping(), "/cholmes/admin-boundaries", Some("list-type=2"),),
        RequestClass::ProxyRequest {
            rewritten_path: "/cholmes--admin-boundaries".to_string(),
            query: Some("list-type=2".to_string()),
        }
    );
}

#[test]
fn account_list_no_prefix() {
    assert_eq!(
        classify_request(&mapping(), "/cholmes", Some("list-type=2")),
        RequestClass::AccountList {
            account: "cholmes".to_string(),
            query: Some("list-type=2".to_string()),
        }
    );
}

#[test]
fn account_list_trailing_slash() {
    assert_eq!(
        classify_request(&mapping(), "/cholmes/", Some("list-type=2")),
        RequestClass::AccountList {
            account: "cholmes".to_string(),
            query: Some("list-type=2".to_string()),
        }
    );
}

#[test]
fn prefix_routed_list() {
    assert_eq!(
        classify_request(
            &mapping(),
            "/cholmes",
            Some("list-type=2&prefix=admin-boundaries/"),
        ),
        RequestClass::ProxyRequest {
            rewritten_path: "/cholmes--admin-boundaries".to_string(),
            query: Some("list-type=2&prefix=".to_string()),
        }
    );
}

#[test]
fn prefix_routed_list_with_subdir() {
    assert_eq!(
        classify_request(
            &mapping(),
            "/cholmes",
            Some("list-type=2&prefix=admin-boundaries/subdir/"),
        ),
        RequestClass::ProxyRequest {
            rewritten_path: "/cholmes--admin-boundaries".to_string(),
            query: Some("list-type=2&prefix=subdir/".to_string()),
        }
    );
}

#[test]
fn single_segment_no_list_query() {
    assert_eq!(
        classify_request(&mapping(), "/cholmes", None),
        RequestClass::BadRequest("Missing product in path".to_string())
    );
}

#[test]
fn url_encoded_prefix() {
    assert_eq!(
        classify_request(
            &mapping(),
            "/cholmes",
            Some("list-type=2&prefix=admin%20boundaries/subdir/"),
        ),
        RequestClass::ProxyRequest {
            rewritten_path: "/cholmes--admin boundaries".to_string(),
            query: Some("list-type=2&prefix=subdir/".to_string()),
        }
    );
}

// ── Query helpers ───────────────────────────────────────────────────

#[test]
fn is_list_request_detects_list_type() {
    assert!(is_list_request("list-type=2"));
    assert!(is_list_request("foo=bar&list-type=2&baz=qux"));
    assert!(!is_list_request("foo=bar"));
    assert!(!is_list_request(""));
}

#[test]
fn extract_query_param_finds_value() {
    assert_eq!(
        extract_query_param("list-type=2&prefix=foo/", "prefix"),
        Some("foo/".to_string())
    );
}

#[test]
fn extract_query_param_missing() {
    assert_eq!(extract_query_param("list-type=2", "prefix"), None);
}

#[test]
fn extract_query_param_decodes_percent() {
    assert_eq!(
        extract_query_param("prefix=hello%20world", "prefix"),
        Some("hello world".to_string())
    );
}

#[test]
fn rewrite_prefix_replaces_value() {
    assert_eq!(
        rewrite_prefix_in_query("list-type=2&prefix=old/", "new/"),
        "list-type=2&prefix=new/"
    );
}

#[test]
fn rewrite_prefix_to_empty() {
    assert_eq!(
        rewrite_prefix_in_query("prefix=old/&max-keys=100", ""),
        "prefix=&max-keys=100"
    );
}
