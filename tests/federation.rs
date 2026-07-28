//! Native unit tests for the wasm-free `federation` module, included via
//! `#[path]` (the lib itself is `cdylib` with `test = false`). Mirrors the
//! pattern in `tests/backend_auth.rs`.

#[path = "../src/federation.rs"]
mod federation;

use chrono::{Duration, Utc};
use federation::{cached, session_name, store};
use multistore::types::BackendCredentials;
use std::sync::Arc;

const SUBJECT: &str = "scv1:conn:abc";

fn creds(expires_in_secs: i64) -> Arc<BackendCredentials> {
    Arc::new(BackendCredentials {
        access_key_id: "ASIAEXAMPLE".into(),
        secret_access_key: "secret".into(),
        session_token: "token".into(),
        expiration: Utc::now() + Duration::seconds(expires_in_secs),
    })
}

// ── credential freshness ───────────────────────────────────────────
//
// The cache is a process-wide static, so each test uses its own role ARN as a
// key rather than clearing shared state.

#[test]
fn returns_credentials_that_are_comfortably_unexpired() {
    store("arn:fresh", SUBJECT, creds(3600));
    assert_eq!(
        cached("arn:fresh", SUBJECT).map(|c| c.access_key_id.clone()),
        Some("ASIAEXAMPLE".to_string())
    );
}

#[test]
fn misses_on_an_unknown_role() {
    assert!(cached("arn:never-stored", SUBJECT).is_none());
}

#[test]
fn treats_expired_credentials_as_a_miss() {
    store("arn:expired", SUBJECT, creds(-1));
    assert!(cached("arn:expired", SUBJECT).is_none());
}

/// The refresh lead is the point of the freshness check: credentials that are
/// *technically* still valid but about to expire must not be handed to a backend
/// request that could outlive them.
#[test]
fn treats_nearly_expired_credentials_as_a_miss() {
    store("arn:nearly-expired", SUBJECT, creds(30));
    assert!(cached("arn:nearly-expired", SUBJECT).is_none());
}

// ── cache key identity ─────────────────────────────────────────────

/// The role's trust policy conditions on the assertion's `sub`, so a second
/// connection pointing at the same role must not be served the first one's
/// credentials — that would succeed where STS would have refused.
#[test]
fn does_not_share_credentials_between_subjects_on_one_role() {
    store("arn:shared-role", "scv1:conn:allowed", creds(3600));
    assert!(cached("arn:shared-role", "scv1:conn:allowed").is_some());
    assert!(cached("arn:shared-role", "scv1:conn:other").is_none());
}

#[test]
fn does_not_share_credentials_between_roles_for_one_subject() {
    store("arn:role-a", SUBJECT, creds(3600));
    assert!(cached("arn:role-b", SUBJECT).is_none());
}

// ── RoleSessionName sanitization ───────────────────────────────────

#[test]
fn replaces_characters_sts_rejects() {
    assert_eq!(session_name("scv1:conn:abc-123"), "scv1_conn_abc-123");
}

#[test]
fn passes_through_an_already_valid_name() {
    assert_eq!(session_name("s3-proxy"), "s3-proxy");
}

#[test]
fn clamps_to_the_64_character_maximum() {
    assert_eq!(session_name(&"a".repeat(100)).len(), 64);
}

#[test]
fn falls_back_when_sanitizing_leaves_too_few_characters() {
    assert_eq!(session_name(":"), "s3-proxy");
}
