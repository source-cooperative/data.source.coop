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
    let result = mapping().rewrite_request("/", None);
    assert_eq!(result.path, "/");
    assert_eq!(result.query, None);
}

#[test]
fn empty_path_passes_through() {
    let result = mapping().rewrite_request("", None);
    assert_eq!(result.path, "");
    assert_eq!(result.query, None);
}

#[test]
fn object_request_two_segments_plus_key() {
    let result = mapping().rewrite_request("/cholmes/admin-boundaries/file.parquet", None);
    assert_eq!(result.path, "/cholmes:admin-boundaries/file.parquet");
    assert_eq!(result.query, None);
    assert_eq!(
        result.signing_path,
        "/cholmes/admin-boundaries/file.parquet"
    );
    assert_eq!(result.signing_query, None);
}

#[test]
fn object_request_nested_key() {
    let result = mapping().rewrite_request("/cholmes/admin-boundaries/dir/sub/file.parquet", None);
    assert_eq!(
        result.path,
        "/cholmes:admin-boundaries/dir/sub/file.parquet"
    );
    assert_eq!(result.query, None);
    assert_eq!(
        result.signing_path,
        "/cholmes/admin-boundaries/dir/sub/file.parquet"
    );
    assert_eq!(result.signing_query, None);
}

#[test]
fn product_list_via_path_segment() {
    let result = mapping().rewrite_request("/cholmes/admin-boundaries", Some("list-type=2"));
    assert_eq!(result.path, "/cholmes:admin-boundaries");
    assert_eq!(result.query, Some("list-type=2".to_string()));
    assert_eq!(result.signing_path, "/cholmes/admin-boundaries");
    assert_eq!(result.signing_query, Some("list-type=2".to_string()));
}

#[test]
fn account_list_passes_through() {
    let result = mapping().rewrite_request("/cholmes", Some("list-type=2"));
    assert_eq!(result.path, "/cholmes");
    assert_eq!(result.query, Some("list-type=2".to_string()));
}

#[test]
fn account_list_trailing_slash_passes_through() {
    let result = mapping().rewrite_request("/cholmes/", Some("list-type=2"));
    assert_eq!(result.path, "/cholmes/");
    assert_eq!(result.query, Some("list-type=2".to_string()));
}

#[test]
fn prefix_routed_list() {
    let result =
        mapping().rewrite_request("/cholmes", Some("list-type=2&prefix=admin-boundaries/"));
    assert_eq!(result.path, "/cholmes:admin-boundaries");
    assert_eq!(result.query, Some("list-type=2&prefix=".to_string()));
    assert_eq!(result.signing_path, "/cholmes");
    assert_eq!(
        result.signing_query,
        Some("list-type=2&prefix=admin-boundaries/".to_string())
    );
}

#[test]
fn prefix_routed_list_with_subdir() {
    let result = mapping().rewrite_request(
        "/cholmes",
        Some("list-type=2&prefix=admin-boundaries/subdir/"),
    );
    assert_eq!(result.path, "/cholmes:admin-boundaries");
    assert_eq!(result.query, Some("list-type=2&prefix=subdir/".to_string()));
    assert_eq!(result.signing_path, "/cholmes");
    assert_eq!(
        result.signing_query,
        Some("list-type=2&prefix=admin-boundaries/subdir/".to_string())
    );
}

#[test]
fn single_segment_no_list_passes_through() {
    let result = mapping().rewrite_request("/cholmes", None);
    assert_eq!(result.path, "/cholmes");
    assert_eq!(result.query, None);
}

#[test]
fn single_segment_with_non_list_query_passes_through() {
    let result = mapping().rewrite_request("/cholmes", Some("not-list-type=2"));
    assert_eq!(result.path, "/cholmes");
    assert_eq!(result.query, Some("not-list-type=2".to_string()));
}

#[test]
fn url_encoded_prefix() {
    let result = mapping().rewrite_request(
        "/cholmes",
        Some("list-type=2&prefix=admin%20boundaries/subdir/"),
    );
    assert_eq!(result.path, "/cholmes:admin boundaries");
    assert_eq!(result.query, Some("list-type=2&prefix=subdir/".to_string()));
    assert_eq!(result.signing_path, "/cholmes");
    assert_eq!(
        result.signing_query,
        Some("list-type=2&prefix=admin%20boundaries/subdir/".to_string())
    );
}
