//! Native unit tests for the wasm-free `sts_cache` helpers, included via
//! `#[path]` (the lib itself is `cdylib` with `test = false`). Mirrors the
//! pattern in `tests/backend_auth.rs`.

#[path = "../src/sts_cache.rs"]
mod sts_cache;

use sts_cache::{cache_inputs_from_form, cache_key, ttl_secs, REFRESH_LEAD_SECS};

const OK_RESP: &str = "<AssumeRoleWithWebIdentityResponse><Credentials>\
    <AccessKeyId>AKID</AccessKeyId>\
    <Expiration>2026-06-30T01:00:00Z</Expiration>\
    </Credentials></AssumeRoleWithWebIdentityResponse>";

fn exp_unix() -> i64 {
    chrono::DateTime::parse_from_rfc3339("2026-06-30T01:00:00Z")
        .unwrap()
        .timestamp()
}

// ── cache_inputs_from_form ─────────────────────────────────────────

#[test]
fn inputs_for_assume_role() {
    let assume = [
        ("Action", "AssumeRoleWithWebIdentity"),
        ("RoleArn", "arn:aws:iam::1:role/r"),
        ("RoleSessionName", "scv1_conn_abc"),
        ("WebIdentityToken", "jwt"),
    ];
    assert_eq!(
        cache_inputs_from_form(&assume),
        Some(("arn:aws:iam::1:role/r", "scv1_conn_abc"))
    );
}

#[test]
fn non_assume_role_action_is_not_cached() {
    // A different action must bypass the cache even if a RoleArn is present.
    let other = [("Action", "GetCallerIdentity"), ("RoleArn", "arn:x")];
    assert_eq!(cache_inputs_from_form(&other), None);
}

#[test]
fn assume_role_without_role_arn_is_none() {
    let no_arn = [
        ("Action", "AssumeRoleWithWebIdentity"),
        ("RoleSessionName", "scv1_conn_abc"),
    ];
    assert_eq!(cache_inputs_from_form(&no_arn), None);
}

#[test]
fn assume_role_without_session_name_is_none() {
    // Missing the per-connection identity → fail closed (bypass cache) rather
    // than key on the role alone and risk cross-connection credential sharing.
    let no_session = [
        ("Action", "AssumeRoleWithWebIdentity"),
        ("RoleArn", "arn:aws:iam::1:role/r"),
    ];
    assert_eq!(cache_inputs_from_form(&no_session), None);
}

// ── cache_key ──────────────────────────────────────────────────────

#[test]
fn cache_key_is_non_routable_and_encodes_inputs() {
    let k = cache_key("arn:aws:iam::1:role/r", "scv1_conn_abc");
    assert!(k.starts_with("https://sts-creds.cache.internal/v1/"));
    // The `:` and `/` in the ARN must be percent-encoded so each part is one
    // well-formed, collision-free path segment.
    assert!(!k
        .trim_start_matches("https://sts-creds.cache.internal/v1/")
        .contains(':'));
    assert_ne!(cache_key("arn:a", "s"), cache_key("arn:b", "s"));
    // Same role, different connection identity → different key. This is the
    // security property: creds are never shared across connections that only
    // share a role.
    assert_ne!(cache_key("arn:r", "connA"), cache_key("arn:r", "connB"));
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

#[test]
fn unparseable_expiration_is_not_cached() {
    // Present but malformed <Expiration> (no TZ) → don't cache a credential
    // whose real expiry we can't determine.
    let bad = "<AssumeRoleWithWebIdentityResponse><Credentials>\
        <Expiration>2026-06-30 01:00:00</Expiration>\
        </Credentials></AssumeRoleWithWebIdentityResponse>";
    assert_eq!(ttl_secs(bad, 0), None);
}
