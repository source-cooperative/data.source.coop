use multistore_path_mapping::PathMapping;

fn mapping() -> PathMapping {
    PathMapping {
        bucket_segments: 2,
        bucket_separator: ":".to_string(),
        display_bucket_segments: 1,
    }
}

// ── rewrite_request ─────────────────────────────────────────────────

#[test]
fn root_path_passes_through() {
    assert_eq!(
        mapping().rewrite_request("/", None),
        ("/".to_string(), None)
    );
}

#[test]
fn empty_path_passes_through() {
    assert_eq!(
        mapping().rewrite_request("", None),
        ("".to_string(), None)
    );
}

#[test]
fn object_request_two_segments_plus_key() {
    assert_eq!(
        mapping().rewrite_request("/cholmes/admin-boundaries/file.parquet", None),
        (
            "/cholmes:admin-boundaries/file.parquet".to_string(),
            None
        )
    );
}

#[test]
fn object_request_nested_key() {
    assert_eq!(
        mapping().rewrite_request(
            "/cholmes/admin-boundaries/dir/sub/file.parquet",
            None
        ),
        (
            "/cholmes:admin-boundaries/dir/sub/file.parquet".to_string(),
            None
        )
    );
}

#[test]
fn product_list_via_path_segment() {
    assert_eq!(
        mapping().rewrite_request(
            "/cholmes/admin-boundaries",
            Some("list-type=2"),
        ),
        (
            "/cholmes:admin-boundaries".to_string(),
            Some("list-type=2".to_string())
        )
    );
}

#[test]
fn account_list_passes_through() {
    assert_eq!(
        mapping().rewrite_request("/cholmes", Some("list-type=2")),
        ("/cholmes".to_string(), Some("list-type=2".to_string()))
    );
}

#[test]
fn account_list_trailing_slash_passes_through() {
    assert_eq!(
        mapping().rewrite_request("/cholmes/", Some("list-type=2")),
        ("/cholmes/".to_string(), Some("list-type=2".to_string()))
    );
}

#[test]
fn prefix_routed_list() {
    assert_eq!(
        mapping().rewrite_request(
            "/cholmes",
            Some("list-type=2&prefix=admin-boundaries/"),
        ),
        (
            "/cholmes:admin-boundaries".to_string(),
            Some("list-type=2&prefix=".to_string())
        )
    );
}

#[test]
fn prefix_routed_list_with_subdir() {
    assert_eq!(
        mapping().rewrite_request(
            "/cholmes",
            Some("list-type=2&prefix=admin-boundaries/subdir/"),
        ),
        (
            "/cholmes:admin-boundaries".to_string(),
            Some("list-type=2&prefix=subdir/".to_string())
        )
    );
}

#[test]
fn single_segment_no_list_passes_through() {
    assert_eq!(
        mapping().rewrite_request("/cholmes", None),
        ("/cholmes".to_string(), None)
    );
}

#[test]
fn single_segment_with_non_list_query_passes_through() {
    assert_eq!(
        mapping().rewrite_request("/cholmes", Some("not-list-type=2")),
        (
            "/cholmes".to_string(),
            Some("not-list-type=2".to_string())
        )
    );
}

#[test]
fn url_encoded_prefix() {
    assert_eq!(
        mapping().rewrite_request(
            "/cholmes",
            Some("list-type=2&prefix=admin%20boundaries/subdir/"),
        ),
        (
            "/cholmes:admin boundaries".to_string(),
            Some("list-type=2&prefix=subdir/".to_string())
        )
    );
}
