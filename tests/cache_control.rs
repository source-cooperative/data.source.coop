#[path = "../src/cache_control.rs"]
mod cache_control;

use cache_control::ensure_no_transform;

// Regression test for https://community.cloudflare.com/t/content-length-header-missing-on-http-3-head-requests/932208
// Responses from data.source.coop must carry `Cache-Control: no-transform` so
// Cloudflare does not strip `Content-Length` on HTTP/3 HEAD requests.

#[test]
fn adds_directive_when_header_absent() {
    assert_eq!(ensure_no_transform(""), Some("no-transform".to_string()));
}

#[test]
fn adds_directive_when_header_is_whitespace() {
    assert_eq!(ensure_no_transform("   "), Some("no-transform".to_string()));
}

#[test]
fn appends_to_existing_directives() {
    assert_eq!(
        ensure_no_transform("max-age=3600"),
        Some("max-age=3600, no-transform".to_string())
    );
}

#[test]
fn idempotent_when_already_present() {
    assert_eq!(ensure_no_transform("no-transform"), None);
    assert_eq!(ensure_no_transform("max-age=60, no-transform"), None);
}

#[test]
fn match_is_case_insensitive() {
    assert_eq!(ensure_no_transform("No-Transform"), None);
}

#[test]
fn ignores_substring_false_positive() {
    // "no-transformation" is not the `no-transform` directive.
    assert_eq!(
        ensure_no_transform("no-transformation"),
        Some("no-transformation, no-transform".to_string())
    );
}
