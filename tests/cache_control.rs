//! Native unit tests for the wasm-free `cache_control` module, included via
//! `#[path]` (the lib itself is `cdylib` with `test = false`). Mirrors the
//! pattern in `tests/authz.rs` and `tests/object_path.rs`.

#[path = "../src/cache_control.rs"]
mod cache_control;

use cache_control::default_cache_control;
use http::Method;

const DEFAULT: Option<&str> = Some("no-cache");
const PATH: &str = "/acct/prod/key.json";

/// A read of a public product.
fn public(method: &Method, status: u16, backend: Option<&str>) -> Option<String> {
    default_cache_control(method, PATH, status, backend, false, false, DEFAULT)
}

/// A read of a non-public product.
fn private(backend: Option<&str>, expires: bool, configured: Option<&str>) -> Option<String> {
    default_cache_control(&Method::GET, PATH, 200, backend, expires, true, configured)
}

fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}

/// The bug from #225: an object read with no `Cache-Control` of its own gets the
/// default, so a cache cannot invent a freshness lifetime from `Last-Modified`.
#[test]
fn read_without_a_backend_header_gets_the_default() {
    assert_eq!(public(&Method::GET, 200, None), s("no-cache"));
    assert_eq!(public(&Method::HEAD, 200, None), s("no-cache"));
}

/// multistore-cf-workers appends `no-transform` to every forwarded response. It
/// is not a freshness policy, so the default still applies, and it is kept.
#[test]
fn no_transform_alone_is_not_a_policy_and_is_kept() {
    assert_eq!(
        public(&Method::GET, 200, Some("no-transform")),
        s("no-cache, no-transform")
    );
    assert_eq!(
        private(Some("no-transform"), false, None),
        s("private, no-cache, no-transform")
    );
}

/// The publisher's own header always wins on public products — overriding it
/// would take immutable assets *out* of cache.
#[test]
fn a_backend_header_is_never_overridden() {
    for existing in [
        "max-age=31536000, immutable",
        "max-age=60, no-transform",
        "no-store",
        "public, max-age=60",
        "no-cache",
    ] {
        assert_eq!(
            public(&Method::GET, 200, Some(existing)),
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

/// A blank header carries no directive, so it counts as absent.
#[test]
fn a_blank_backend_header_is_treated_as_absent() {
    for existing in ["", "   ", "\t"] {
        assert_eq!(
            public(&Method::GET, 200, Some(existing)),
            s("no-cache"),
            "blank value {existing:?} should not suppress the default"
        );
    }
}

#[test]
fn writes_are_left_alone() {
    for method in [Method::PUT, Method::POST, Method::DELETE, Method::PATCH] {
        assert_eq!(public(&method, 200, None), None, "{method}");
    }
}

/// RFC 9111 §4.3.4: a cache updates the *stored* response's headers from a 304.
#[test]
fn not_modified_is_left_alone() {
    assert_eq!(public(&Method::GET, 304, None), None);
    assert_eq!(public(&Method::HEAD, 304, None), None);
}

/// Heuristic freshness covers 206, 404 and 410 too.
#[test]
fn other_read_statuses_get_the_default() {
    for status in [200, 206, 301, 404, 410, 500] {
        assert_eq!(
            public(&Method::GET, status, None),
            s("no-cache"),
            "{status}"
        );
    }
}

#[test]
fn unconfigured_disables_the_default() {
    assert_eq!(
        default_cache_control(&Method::GET, PATH, 200, None, false, false, None),
        None
    );
}

#[test]
fn the_configured_value_is_used_verbatim() {
    assert_eq!(
        default_cache_control(
            &Method::GET,
            PATH,
            200,
            None,
            false,
            false,
            Some("public, max-age=300")
        ),
        s("public, max-age=300")
    );
}

/// A non-public product is `private` whatever the operator configured —
/// including a disabled default — so a shared cache never keeps its bytes.
#[test]
fn non_public_products_are_always_private() {
    assert_eq!(
        private(None, false, Some("public, max-age=300")),
        s("private, no-cache")
    );
    assert_eq!(private(None, false, None), s("private, no-cache"));
}

/// A publisher's `public`/`s-maxage` on a non-public product is dropped; the
/// rest of their policy is kept, now `private`.
#[test]
fn non_public_backend_headers_are_made_private() {
    assert_eq!(
        private(Some("public, max-age=86400, s-maxage=600"), false, DEFAULT),
        s("private, max-age=86400")
    );
    assert_eq!(
        private(Some("Private, no-store"), false, DEFAULT),
        s("private, no-store")
    );
    assert_eq!(
        private(Some("max-age=60, no-transform"), false, DEFAULT),
        s("private, max-age=60, no-transform")
    );
    // `Expires` alone stays in charge of freshness; `private` keeps it out of
    // shared caches.
    assert_eq!(private(None, true, DEFAULT), s("private"));
}

/// `/.sts` returns credentials, so it is `no-store` for every method and status,
/// even with the default disabled. Near-miss paths are ordinary reads.
#[test]
fn sts_is_never_stored() {
    for (method, status) in [(Method::GET, 200), (Method::POST, 200), (Method::GET, 501)] {
        assert_eq!(
            default_cache_control(&method, "/.sts", status, None, false, false, None),
            s("no-store"),
            "{method} {status}"
        );
    }
    assert_eq!(
        default_cache_control(&Method::GET, "/.stsx", 200, None, false, false, DEFAULT),
        s("no-cache")
    );
}
