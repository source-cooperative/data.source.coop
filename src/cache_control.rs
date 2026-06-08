//! `Cache-Control` header handling for proxied responses.

/// Computes a `Cache-Control` value that guarantees a `no-transform`
/// directive is present, preserving any directives already set by the
/// upstream object (e.g. `max-age`).
///
/// Cloudflare otherwise applies transformations that strip the
/// `Content-Length` header on HTTP/3 HEAD requests:
/// <https://community.cloudflare.com/t/content-length-header-missing-on-http-3-head-requests/932208>
///
/// Returns `None` when `existing` already contains a `no-transform`
/// directive (case-insensitive), signalling that the header needs no change.
pub fn ensure_no_transform(existing: &str) -> Option<String> {
    let already_present = existing
        .split(',')
        .any(|directive| directive.trim().eq_ignore_ascii_case("no-transform"));
    if already_present {
        return None;
    }

    let existing = existing.trim();
    if existing.is_empty() {
        Some("no-transform".to_string())
    } else {
        Some(format!("{existing}, no-transform"))
    }
}
