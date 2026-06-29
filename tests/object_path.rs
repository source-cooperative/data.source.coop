//! Native unit tests for the wasm-free `object_path` module, included via
//! `#[path]` (the lib itself is `cdylib` with `test = false`). Mirrors the
//! pattern in `tests/authz.rs`.

#[path = "../src/object_path.rs"]
mod object_path;

use http::Method;
use object_path::{extract_path_segments, is_keyless_write};

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
