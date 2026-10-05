//! Native unit tests for the wasm-free `cache_control` module, included via
//! `#[path]` (the lib itself is `cdylib` with `test = false`). Mirrors the
//! pattern in `tests/authz.rs` and `tests/object_path.rs`.

#[path = "../src/cache_control.rs"]
mod cache_control;

use cache_control::{default_cache_control, is_credentialed};
use http::{HeaderMap, HeaderValue, Method};

const DEFAULT: Option<&str> = Some("no-cache");
const PATH: &str = "/acct/prod/key.json";

/// Anonymous read through the common path.
fn anon(method: &Method, status: u16, backend: Option<&str>) -> Option<&'static str> {
    default_cache_control(method, PATH, status, backend, false, false, DEFAULT)
}

/// The bug from #225: an object read with no `Cache-Control` of its own gets the
/// default, so a cache cannot invent a freshness lifetime from `Last-Modified`.
#[test]
fn read_without_a_backend_header_gets_the_default() {
    assert_eq!(anon(&Method::GET, 200, None), DEFAULT);
    assert_eq!(anon(&Method::HEAD, 200, None), DEFAULT);
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
            anon(&Method::GET, 200, Some(existing)),
            None,
            "should not override backend value {existing:?}"
        );
    }
}

/// `Expires` is a backend freshness policy too; `no-cache` would override it.
#[test]
fn a_backend_expires_is_never_overridden() {
    assert_eq!(
        default_cache_control(&Method::GET, PATH, 200, None, true, false, DEFAULT),
        None
    );
}

/// A present-but-blank header carries no directive, so heuristic freshness
/// applies exactly as if it were absent. Treat it as absent.
#[test]
fn a_blank_backend_header_is_treated_as_absent() {
    for existing in ["", "   ", "\t"] {
        assert_eq!(
            anon(&Method::GET, 200, Some(existing)),
            DEFAULT,
            "blank value {existing:?} should not suppress the default"
        );
    }
}

/// Writes are not heuristically cacheable; adding the header would be noise.
#[test]
fn writes_are_left_alone() {
    for method in [Method::PUT, Method::POST, Method::DELETE, Method::PATCH] {
        assert_eq!(
            anon(&method, 200, None),
            None,
            "{method} should not get a default"
        );
    }
}

/// RFC 9111 §4.3.4: a cache updates the *stored* response's headers from a 304,
/// so injecting there would overwrite a publisher's stored directive.
#[test]
fn not_modified_is_left_alone() {
    assert_eq!(anon(&Method::GET, 304, None), None);
    assert_eq!(anon(&Method::HEAD, 304, None), None);
}

/// Every other read status still gets it: heuristic freshness covers 206, 404
/// and 410 too, so a missing object must not be cached as missing for a day.
#[test]
fn other_read_statuses_get_the_default() {
    for status in [200, 206, 301, 404, 410, 500] {
        assert_eq!(
            anon(&Method::GET, status, None),
            DEFAULT,
            "{status} should get a default"
        );
    }
}

/// `None` (a blank `DEFAULT_CACHE_CONTROL`) disables the default.
#[test]
fn unconfigured_disables_the_default() {
    assert_eq!(
        default_cache_control(&Method::GET, PATH, 200, None, false, false, None),
        None
    );
}

/// The value is passed through verbatim for anonymous reads, so an operator can
/// set a real `max-age` policy without touching the code.
#[test]
fn the_configured_value_is_used_verbatim() {
    let configured = Some("public, max-age=300");
    assert_eq!(
        default_cache_control(&Method::GET, PATH, 200, None, false, false, configured),
        configured
    );
}

/// A credentialed read never gets the configured value: `public, max-age=300`
/// would let a shared cache keep a restricted product's bytes.
#[test]
fn credentialed_reads_are_private() {
    let configured = Some("public, max-age=300");
    assert_eq!(
        default_cache_control(&Method::GET, PATH, 200, None, false, true, configured),
        Some("private, no-cache")
    );
    // Still disabled when unconfigured, and still never overrides the backend.
    assert_eq!(
        default_cache_control(&Method::GET, PATH, 200, None, false, true, None),
        None
    );
    assert_eq!(
        default_cache_control(
            &Method::GET,
            PATH,
            200,
            Some("no-store"),
            false,
            true,
            configured
        ),
        None
    );
}

/// `/.sts` returns credentials, so it is `no-store` for every method and status,
/// even with the default disabled. Near-miss paths are ordinary reads.
#[test]
fn sts_is_never_stored() {
    for (method, status) in [(Method::GET, 200), (Method::POST, 200), (Method::GET, 501)] {
        assert_eq!(
            default_cache_control(&method, "/.sts", status, None, false, false, None),
            Some("no-store"),
            "{method} {status}"
        );
    }
    assert_eq!(
        default_cache_control(&Method::GET, "/.stsx", 200, None, false, false, DEFAULT),
        DEFAULT
    );
}

#[test]
fn credentials_are_detected() {
    let mut headers = HeaderMap::new();
    assert!(!is_credentialed(&headers, None));
    assert!(!is_credentialed(&headers, Some("list-type=2&prefix=a/")));
    assert!(is_credentialed(
        &headers,
        Some("X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Signature=abc")
    ));
    assert!(is_credentialed(&headers, Some("x-amz-signature=abc")));
    headers.insert(
        "authorization",
        HeaderValue::from_static("AWS4-HMAC-SHA256 ..."),
    );
    assert!(is_credentialed(&headers, None));
}
