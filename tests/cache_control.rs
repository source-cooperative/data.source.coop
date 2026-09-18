//! Native unit tests for the wasm-free `cache_control` module, included via
//! `#[path]` (the lib itself is `cdylib` with `test = false`). Mirrors the
//! pattern in `tests/authz.rs` and `tests/object_path.rs`.

#[path = "../src/cache_control.rs"]
mod cache_control;

use cache_control::default_cache_control;
use http::Method;

const DEFAULT: &str = "no-cache";

/// The bug from #225: an object read with no `Cache-Control` of its own gets the
/// default, so a cache cannot invent a freshness lifetime from `Last-Modified`.
#[test]
fn read_without_a_backend_header_gets_the_default() {
    assert_eq!(
        default_cache_control(&Method::GET, 200, None, DEFAULT),
        Some(DEFAULT)
    );
    assert_eq!(
        default_cache_control(&Method::HEAD, 200, None, DEFAULT),
        Some(DEFAULT)
    );
}

/// The publisher's own header always wins — that is the whole point of option 1
/// in #225. Overriding it would take immutable assets *out* of cache.
#[test]
fn a_backend_header_is_never_overridden() {
    for existing in [
        "max-age=31536000, immutable",
        "no-store",
        "public, max-age=60",
        // Even a value identical to the default is left alone rather than reset.
        "no-cache",
    ] {
        assert_eq!(
            default_cache_control(&Method::GET, 200, Some(existing), DEFAULT),
            None,
            "should not override backend value {existing:?}"
        );
    }
}

/// A present-but-blank header carries no directive, so heuristic freshness
/// applies exactly as if it were absent. Treat it as absent.
#[test]
fn a_blank_backend_header_is_treated_as_absent() {
    for existing in ["", "   ", "\t"] {
        assert_eq!(
            default_cache_control(&Method::GET, 200, Some(existing), DEFAULT),
            Some(DEFAULT),
            "blank value {existing:?} should not suppress the default"
        );
    }
}

/// Writes are not heuristically cacheable; adding the header would be noise.
#[test]
fn writes_are_left_alone() {
    for method in [Method::PUT, Method::POST, Method::DELETE, Method::PATCH] {
        assert_eq!(
            default_cache_control(&method, 200, None, DEFAULT),
            None,
            "{method} should not get a default"
        );
    }
}

/// RFC 9111 §4.3.4: a cache updates the *stored* response's headers from a 304.
/// Injecting there would overwrite a publisher's `max-age=31536000, immutable`
/// — stored from the original 200 — with our default, turning the passthrough
/// guarantee into a one-round-trip delay rather than a rule.
#[test]
fn not_modified_is_left_alone() {
    assert_eq!(
        default_cache_control(&Method::GET, 304, None, DEFAULT),
        None
    );
    assert_eq!(
        default_cache_control(&Method::HEAD, 304, None, DEFAULT),
        None
    );
}

/// Every other read status still gets it: heuristic freshness covers 206, 404
/// and 410 too, so a missing object must not be cached as missing for a day.
#[test]
fn other_read_statuses_get_the_default() {
    for status in [200, 206, 301, 404, 410, 500] {
        assert_eq!(
            default_cache_control(&Method::GET, status, None, DEFAULT),
            Some(DEFAULT),
            "{status} should get a default"
        );
    }
}

/// The empty string is the documented escape hatch: send no header at all,
/// restoring the pre-#225 behaviour without a code change. An all-whitespace
/// value means the same thing — emitting `cache-control: ` would leave the
/// response directive-less, which is the bug, not the escape hatch.
#[test]
fn blank_configuration_disables_the_default() {
    for configured in ["", " ", "\t", "\n"] {
        assert_eq!(
            default_cache_control(&Method::GET, 200, None, configured),
            None,
            "configured {configured:?} should disable the default"
        );
    }
}

/// The value is passed through verbatim (bar surrounding whitespace), so an
/// operator can set a real `max-age` policy without touching the code.
#[test]
fn the_configured_value_is_used_verbatim() {
    assert_eq!(
        default_cache_control(&Method::GET, 200, None, "public, max-age=300"),
        Some("public, max-age=300")
    );
    assert_eq!(
        default_cache_control(&Method::GET, 200, None, "  public, max-age=300  "),
        Some("public, max-age=300")
    );
}
