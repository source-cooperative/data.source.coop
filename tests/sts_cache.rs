//! Native unit tests for the wasm-free `sts_cache` helpers, included via
//! `#[path]` (the lib itself is `cdylib` with `test = false`). Mirrors the
//! pattern in `tests/backend_auth.rs`.

#[path = "../src/sts_cache.rs"]
mod sts_cache;

use sts_cache::{cache_key, role_arn_from_form, ttl_secs, REFRESH_LEAD_SECS};

const OK_RESP: &str = "<AssumeRoleWithWebIdentityResponse><Credentials>\
    <AccessKeyId>AKID</AccessKeyId>\
    <Expiration>2026-06-30T01:00:00Z</Expiration>\
    </Credentials></AssumeRoleWithWebIdentityResponse>";

fn exp_unix() -> i64 {
    chrono::DateTime::parse_from_rfc3339("2026-06-30T01:00:00Z")
        .unwrap()
        .timestamp()
}

// ── role_arn_from_form ─────────────────────────────────────────────

#[test]
fn role_arn_only_for_assume_role() {
    let assume = [
        ("Action", "AssumeRoleWithWebIdentity"),
        ("RoleArn", "arn:aws:iam::1:role/r"),
        ("WebIdentityToken", "jwt"),
    ];
    assert_eq!(role_arn_from_form(&assume), Some("arn:aws:iam::1:role/r"));
}

#[test]
fn non_assume_role_action_is_not_cached() {
    // A different action must bypass the cache even if a RoleArn is present.
    let other = [("Action", "GetCallerIdentity"), ("RoleArn", "arn:x")];
    assert_eq!(role_arn_from_form(&other), None);
}

#[test]
fn assume_role_without_role_arn_is_none() {
    let no_arn = [("Action", "AssumeRoleWithWebIdentity")];
    assert_eq!(role_arn_from_form(&no_arn), None);
}

// ── cache_key ──────────────────────────────────────────────────────

#[test]
fn cache_key_is_non_routable_and_encodes_arn() {
    let k = cache_key("arn:aws:iam::1:role/r");
    assert!(k.starts_with("https://sts-creds.cache.internal/v1/"));
    // The `:` and `/` in the ARN must be percent-encoded so the key is one
    // well-formed, collision-free path segment.
    assert!(!k
        .trim_start_matches("https://sts-creds.cache.internal/v1/")
        .contains(':'));
    assert_ne!(cache_key("arn:a"), cache_key("arn:b"));
}

// ── ttl_secs ───────────────────────────────────────────────────────

#[test]
fn ttl_is_time_to_expiry_minus_lead() {
    let now = exp_unix() - 3600; // one hour before expiry
    assert_eq!(
        ttl_secs(OK_RESP, now),
        Some((3600 - REFRESH_LEAD_SECS) as u32)
    );
}

#[test]
fn near_expiry_is_not_cached() {
    let now = exp_unix() - 60; // inside the 300s lead window
    assert_eq!(ttl_secs(OK_RESP, now), None);
}

#[test]
fn expired_is_not_cached() {
    let now = exp_unix() + 10; // already past expiry
    assert_eq!(ttl_secs(OK_RESP, now), None);
}

#[test]
fn sts_error_document_is_not_cached() {
    // No <Expiration> → never cache a failure as if it were a credential.
    let err = "<ErrorResponse><Error><Code>AccessDenied</Code>\
        <Message>nope</Message></Error></ErrorResponse>";
    assert_eq!(ttl_secs(err, 0), None);
}
