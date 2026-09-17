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
        default_cache_control(&Method::GET, "/acct/prod/collection.json", None, DEFAULT),
        Some(DEFAULT)
    );
    assert_eq!(
        default_cache_control(&Method::HEAD, "/acct/prod/collection.json", None, DEFAULT),
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
            default_cache_control(
                &Method::GET,
                "/acct/prod/data.parquet",
                Some(existing),
                DEFAULT
            ),
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
            default_cache_control(&Method::GET, "/acct/prod/x.json", Some(existing), DEFAULT),
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
            default_cache_control(&method, "/acct/prod/x.json", None, DEFAULT),
            None,
            "{method} should not get a default"
        );
    }
}

/// Control-plane endpoints manage their own caching. OIDC discovery in
/// particular is served by `multistore-oidc-provider`, and JWKS caching is a
/// key-rotation concern, not a freshness one.
#[test]
fn control_plane_endpoints_are_left_alone() {
    for path in [
        "/.well-known/openid-configuration",
        "/.well-known/jwks.json",
        "/.sts",
        // `lib.rs` normalizes this before routing, but the policy accepts both
        // so it cannot depend on call ordering.
        "/.sts/",
    ] {
        assert_eq!(
            default_cache_control(&Method::GET, path, None, DEFAULT),
            None,
            "{path} should not get a default"
        );
    }
}

/// A product whose account happens to start with a dot is still a data read —
/// the control-plane test is on specific paths, not a bare `/.` prefix, so this
/// must not be swept up with it.
#[test]
fn only_the_real_control_plane_paths_are_exempt() {
    assert_eq!(
        default_cache_control(&Method::GET, "/.well-knownish/x", None, DEFAULT),
        Some(DEFAULT)
    );
    assert_eq!(
        default_cache_control(&Method::GET, "/.stsx", None, DEFAULT),
        Some(DEFAULT)
    );
}

/// The empty string is the documented escape hatch: send no header at all,
/// restoring the pre-#225 behaviour without a code change.
#[test]
fn empty_configuration_disables_the_default() {
    assert_eq!(
        default_cache_control(&Method::GET, "/acct/prod/x.json", None, ""),
        None
    );
}

/// The value is passed through verbatim, so an operator can set a real
/// `max-age` policy without touching the code.
#[test]
fn the_configured_value_is_used_verbatim() {
    assert_eq!(
        default_cache_control(
            &Method::GET,
            "/acct/prod/x.json",
            None,
            "public, max-age=300"
        ),
        Some("public, max-age=300")
    );
}

/// Listings and the account index are reads too, and equally subject to
/// heuristic freshness.
#[test]
fn listings_and_index_get_the_default() {
    for path in ["/", "/acct", "/acct/prod/"] {
        assert_eq!(
            default_cache_control(&Method::GET, path, None, DEFAULT),
            Some(DEFAULT),
            "{path} should get a default"
        );
    }
}
