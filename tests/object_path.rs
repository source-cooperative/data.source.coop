//! Native unit tests for the wasm-free `object_path` module, included via
//! `#[path]` (the lib itself is `cdylib` with `test = false`). Mirrors the
//! pattern in `tests/authz.rs`.

#[path = "../src/object_path.rs"]
mod object_path;

use http::Method;
use object_path::{extract_path_segments, is_keyless_write, mapped_copy_source};

/// The deployment's real mapping: `/{account}/{product}/{key}` folds the first
/// two segments into the internal bucket `account:product`.
fn mapping() -> multistore_path_mapping::PathMapping {
    multistore_path_mapping::PathMapping {
        bucket_segments: 2,
        bucket_separator: ":".to_string(),
        display_bucket_segments: 1,
    }
}

fn headers(copy_source: Option<&str>) -> http::HeaderMap {
    let mut h = http::HeaderMap::new();
    if let Some(v) = copy_source {
        h.insert("x-amz-copy-source", v.parse().unwrap());
    }
    h
}

/// The whole point: a client-coordinate copy source (`/account/product/key`)
/// becomes a registry-coordinate one (`account:product/key`). Without this the
/// source names a bucket the registry has never heard of and the copy 404s.
#[test]
fn copy_source_is_folded_into_the_internal_bucket_name() {
    assert_eq!(
        mapped_copy_source(&headers(Some("/acct/prod/README.md")), &mapping()),
        Some("/acct:prod/README.md".to_string())
    );
    // Leading slash is optional in `x-amz-copy-source`; both forms map alike.
    assert_eq!(
        mapped_copy_source(&headers(Some("acct/prod/README.md")), &mapping()),
        Some("/acct:prod/README.md".to_string())
    );
    // Nested keys keep every segment past the bucket.
    assert_eq!(
        mapped_copy_source(&headers(Some("/acct/prod/a/b/c.txt")), &mapping()),
        Some("/acct:prod/a/b/c.txt".to_string())
    );
}

/// `versionId` rides along untouched — multistore#129 authorizes the source
/// against that version, so losing it here would silently copy the wrong one.
/// Percent-encoding is likewise preserved rather than decoded.
#[test]
fn copy_source_preserves_version_and_encoding() {
    assert_eq!(
        mapped_copy_source(
            &headers(Some("/acct/prod/a%20b.txt?versionId=v42")),
            &mapping()
        ),
        Some("/acct:prod/a%20b.txt?versionId=v42".to_string())
    );
}

/// No header means the request isn't a copy: nothing to map, and multistore
/// must be left reading the (absent) header itself rather than handed a value.
#[test]
fn absent_copy_source_maps_to_none() {
    assert_eq!(mapped_copy_source(&headers(None), &mapping()), None);
}

/// Too few segments to name an object inside a product. Mapping these would
/// invent a bucket name; `None` lets them fail on their own merits instead.
#[test]
fn unmappable_copy_source_maps_to_none() {
    assert_eq!(
        mapped_copy_source(&headers(Some("/acct/prod")), &mapping()),
        None
    );
    assert_eq!(
        mapped_copy_source(&headers(Some("/acct")), &mapping()),
        None
    );
}

#[test]
fn extract_splits_account_product_key() {
    assert_eq!(extract_path_segments("/"), (None, None, None));
    assert_eq!(extract_path_segments("/acct"), (Some("acct"), None, None));
    assert_eq!(
        extract_path_segments("/acct/prod"),
        (Some("acct"), Some("prod"), None)
    );
    assert_eq!(
        extract_path_segments("/acct/prod/README.md"),
        (Some("acct"), Some("prod"), Some("README.md"))
    );
    // Nested keys stay intact.
    assert_eq!(
        extract_path_segments("/acct/prod/dir/sub/f.parquet"),
        (Some("acct"), Some("prod"), Some("dir/sub/f.parquet"))
    );
    // A trailing slash is not a key.
    assert_eq!(
        extract_path_segments("/acct/prod/"),
        (Some("acct"), Some("prod"), None)
    );
}

#[test]
fn keyless_writes_are_flagged() {
    // The reported bug: PUT to the product root (no trailing slash, no key).
    assert!(is_keyless_write(&Method::PUT, "/acct/prod"));
    assert!(is_keyless_write(&Method::PUT, "/acct/prod/"));
    assert!(is_keyless_write(&Method::PUT, "/acct"));
    assert!(is_keyless_write(&Method::PUT, "/"));
    // DELETE shares the same failure mode (DeleteObject needs a key).
    assert!(is_keyless_write(&Method::DELETE, "/acct/prod"));
}

#[test]
fn writes_with_a_key_are_allowed() {
    assert!(!is_keyless_write(&Method::PUT, "/acct/prod/README.md"));
    assert!(!is_keyless_write(
        &Method::DELETE,
        "/acct/prod/dir/f.parquet"
    ));
}

#[test]
fn reads_and_multi_delete_are_not_flagged() {
    // Reads to the product root are legitimate (account/product listings).
    assert!(!is_keyless_write(&Method::GET, "/acct/prod"));
    assert!(!is_keyless_write(&Method::HEAD, "/acct"));
    // Keyless POST is left to the gateway: `POST /{account}/{product}?delete`
    // (multi-object delete) is a valid bucket-level operation.
    assert!(!is_keyless_write(&Method::POST, "/acct/prod"));
}
