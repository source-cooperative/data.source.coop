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

/// Map an inbound `x-amz-copy-source` header into the registry's namespace.
///
/// `CopyObject` names its source in a header rather than the URL, in client
/// coordinates (`/{account}/{product}/{key}`). The registry only knows mapped
/// bucket names (`account:product`), so an unmapped source resolves to a bucket
/// it has never heard of and the copy fails with 404 NoSuchBucket. The client
/// signed the header, so it must not be mutated — the mapped value produced
/// here is passed alongside it via `RequestInfo::with_copy_source`, and
/// signature verification keeps using the header as sent.
///
/// Returns `None` when there is no copy-source header (the request isn't a
/// copy) or when the value can't be mapped — too few segments to name an
/// object, or not valid UTF-8. `None` leaves multistore reading the header
/// as-is, which is the correct fallback: a non-copy request has nothing to map,
/// and an unmappable source should fail on its own merits rather than on a
/// silently-invented bucket name.
pub(crate) fn mapped_copy_source(
    headers: &http::HeaderMap,
    mapping: &multistore_path_mapping::PathMapping,
) -> Option<String> {
    headers
        .get("x-amz-copy-source")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| mapping.rewrite_copy_source(v))
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
        && extract_path_segments(path).2.is_none()
}
