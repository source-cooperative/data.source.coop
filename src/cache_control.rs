//! Default `Cache-Control` for proxied responses.
//!
//! Kept wasm-free so the policy can be unit-tested natively (see
//! `tests/cache_control.rs`), despite the crate's `[lib] test = false`.
//!
//! # Why a default is needed
//!
//! Backends here generally do not set `Cache-Control` on their objects, and the
//! proxy relays response headers through a denylist
//! (`multistore::route_handler::RESPONSE_HEADER_DENYLIST`) that does not include
//! `cache-control` — so an object that *does* carry one already passes through
//! untouched, and an object that does not leaves the response with no freshness
//! information at all.
//!
//! [RFC 9111 §4.2.2] then lets a cache invent its own freshness lifetime, and the
//! common heuristic is 10% of the time since `Last-Modified` — which the proxy
//! does send. A file untouched for ten days is treated as fresh for a day, so a
//! browser can serve a pre-publish body, with a stale `ETag`, as a plain 200. The
//! caller cannot tell. See #225 for the STAC catalog this broke.
//!
//! [RFC 9111 §4.2.2]: https://www.rfc-editor.org/rfc/rfc9111#section-4.2.2
//!
//! # Scope
//!
//! This governs what the proxy tells *downstream* caches. It is unrelated to
//! whether the worker's own Cache API stores object bytes (#188): a `Range`
//! response is a 206, which `cache.put` refuses and which multistore explicitly
//! keeps out of the subrequest cache, so no value of this header makes ranged
//! reads edge-cacheable.

/// Decide the `Cache-Control` value to add to a response, or `None` to leave the
/// response untouched.
///
/// Returns `Some(configured)` only when all of the following hold:
///
/// * `configured` is non-empty — the empty string disables the default entirely,
///   restoring the pre-#225 behaviour of sending no header.
/// * The request is a read (`GET`/`HEAD`). Responses to writes are not
///   heuristically cacheable and gain nothing from the header.
/// * The path is not a control-plane endpoint (`/.well-known/*`, `/.sts`). Those
///   manage their own caching — OIDC discovery in particular is served by
///   `multistore-oidc-provider`, and overriding it here would be a surprise.
/// * The response does not already carry a `Cache-Control`. **Passthrough always
///   wins**: a publisher who sets `max-age=31536000, immutable` on an immutable
///   object, or `no-cache` on one they overwrite in place, has said what they
///   want and the proxy must not second-guess it.
///
/// Deliberately applied to *every* read status, not just 200. RFC 9111 §4.2.2
/// heuristic freshness also covers 206, 404 and 410, so a missing object can be
/// cached as missing for as long as a stale body can be cached as fresh.
pub(crate) fn default_cache_control<'a>(
    method: &http::Method,
    path: &str,
    existing: Option<&str>,
    configured: &'a str,
) -> Option<&'a str> {
    if configured.is_empty() {
        return None;
    }
    if !matches!(*method, http::Method::GET | http::Method::HEAD) {
        return None;
    }
    if is_control_plane(path) {
        return None;
    }
    // Treat a present-but-empty header as absent: it carries no directive, so
    // RFC 9111 heuristic freshness applies exactly as if it were missing.
    if existing.is_some_and(|v| !v.trim().is_empty()) {
        return None;
    }
    Some(configured)
}

/// Whether a path is a proxy control-plane endpoint rather than a data read.
///
/// Matches the `is_special_path` test in `lib.rs`, plus the trailing-slash
/// `/.sts/` form that `lib.rs` normalizes before routing — this function runs on
/// the pre-normalization path in some call orders, so it accepts both.
fn is_control_plane(path: &str) -> bool {
    path.starts_with("/.well-known/") || path == "/.sts" || path == "/.sts/"
}
