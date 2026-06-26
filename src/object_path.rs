//! Object-path parsing for the Source Cooperative path model
//! (`/{account}/{product}/{key}`). Kept wasm-free so it can be unit-tested
//! natively (see `tests/object_path.rs`), despite the crate's `[lib] test = false`.

/// Split a request path into `(account, product, key)`.
///
/// The key is everything after the first two segments, so nested keys stay
/// intact. Leading/trailing slashes are ignored, so `/a/b` and `/a/b/` both
/// parse with `key = None`. Examples:
///   `/`                  → (None, None, None)
///   `/acct`              → (Some("acct"), None, None)
///   `/acct/prod`         → (Some("acct"), Some("prod"), None)
///   `/acct/prod/dir/f`   → (Some("acct"), Some("prod"), Some("dir/f"))
pub(crate) fn extract_path_segments(path: &str) -> (Option<&str>, Option<&str>, Option<&str>) {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return (None, None, None);
    }
    let mut parts = trimmed.splitn(3, '/');
    let account = parts.next();
    let product = parts.next();
    let key = parts.next();
    (account, product, key)
}

/// Whether `method` writes to a single object but `path` carries no object key.
///
/// `PUT`/`DELETE` (PutObject/DeleteObject) address one object, so they need a
/// key: `/{account}/{product}/{key}`. A request to `/{account}/{product}` (or
/// shorter) targets the product root, which has no key — e.g.
/// `aws s3 cp f s3://account/product` (no trailing slash) uploads `f` as the
/// object literally named `product`. Such a request can't be served and, if
/// forwarded, the upstream rejects the streaming upload with a misleading
/// "x-amz-content-sha256 header is invalid"; callers should be told the real
/// cause instead.
///
/// `POST` is intentionally excluded: keyless `POST /{account}/{product}?delete`
/// (multi-object delete) is a legitimate bucket-level operation.
pub(crate) fn is_keyless_write(method: &http::Method, path: &str) -> bool {
    (*method == http::Method::PUT || *method == http::Method::DELETE)
        && extract_path_segments(path).2.is_none_or(str::is_empty)
}
