#[path = "../src/redirect.rs"]
mod redirect;

use redirect::{build_redirect_path, extract_redirect_segments, permanent_redirect_xml};

// ── build_redirect_path ──────────────────────────────────────────

#[test]
fn redirect_object_request() {
    let result = build_redirect_path(
        "/old-account/some-product/file.parquet",
        None,
        "new-account",
    );
    assert_eq!(result, "/new-account/some-product/file.parquet");
}

#[test]
fn redirect_object_request_with_query() {
    let result = build_redirect_path(
        "/old-account/some-product/file.parquet",
        Some("versionId=123"),
        "new-account",
    );
    assert_eq!(
        result,
        "/new-account/some-product/file.parquet?versionId=123"
    );
}

#[test]
fn redirect_product_list() {
    let result = build_redirect_path(
        "/old-account/some-product",
        Some("list-type=2&prefix=subdir/"),
        "new-account",
    );
    assert_eq!(
        result,
        "/new-account/some-product?list-type=2&prefix=subdir/"
    );
}

#[test]
fn redirect_account_list() {
    let result = build_redirect_path("/old-account", Some("list-type=2"), "new-account");
    assert_eq!(result, "/new-account?list-type=2");
}

#[test]
fn redirect_account_list_no_query() {
    let result = build_redirect_path("/old-account", None, "new-account");
    assert_eq!(result, "/new-account");
}

#[test]
fn redirect_nested_key() {
    let result = build_redirect_path("/old-account/product/dir/sub/file.txt", None, "new-account");
    assert_eq!(result, "/new-account/product/dir/sub/file.txt");
}

#[test]
fn redirect_preserves_trailing_slash() {
    let result = build_redirect_path("/old-account/", Some("list-type=2"), "new-account");
    assert_eq!(result, "/new-account/?list-type=2");
}

// ── permanent_redirect_xml ───────────────────────────────────────

#[test]
fn xml_contains_permanent_redirect_code() {
    let xml = permanent_redirect_xml("new-account", "req-123");
    assert!(xml.contains("<Code>PermanentRedirect</Code>"));
}

#[test]
fn xml_contains_bucket_name() {
    let xml = permanent_redirect_xml("new-account", "req-123");
    assert!(xml.contains("<Bucket>new-account</Bucket>"));
}

#[test]
fn xml_contains_request_id() {
    let xml = permanent_redirect_xml("new-account", "req-123");
    assert!(xml.contains("<RequestId>req-123</RequestId>"));
}

#[test]
fn xml_contains_endpoint() {
    let xml = permanent_redirect_xml("new-account", "req-123");
    assert!(xml.contains("<Endpoint>data.source.coop</Endpoint>"));
}

// ── extract_redirect_segments ────────────────────────────────────

#[test]
fn segments_empty_path() {
    assert_eq!(extract_redirect_segments("/"), (None, None));
}

#[test]
fn segments_account_only() {
    assert_eq!(
        extract_redirect_segments("/cholmes"),
        (Some("cholmes"), None)
    );
}

#[test]
fn segments_account_and_product() {
    assert_eq!(
        extract_redirect_segments("/cholmes/admin-boundaries"),
        (Some("cholmes"), Some("admin-boundaries"))
    );
}

#[test]
fn segments_account_product_and_key() {
    assert_eq!(
        extract_redirect_segments("/cholmes/admin-boundaries/file.parquet"),
        (Some("cholmes"), Some("admin-boundaries"))
    );
}

#[test]
fn segments_account_trailing_slash() {
    assert_eq!(
        extract_redirect_segments("/cholmes/"),
        (Some("cholmes"), None)
    );
}
