//! Native unit tests for the wasm-free `federation` module, included via
//! `#[path]` (the lib itself is `cdylib` with `test = false`). Mirrors the
//! pattern in `tests/backend_auth.rs`.

#[path = "../src/federation.rs"]
mod federation;

use chrono::{Duration, Utc};
use federation::{lookup, session_name, store, Action};
use multistore::types::BackendCredentials;
use std::sync::Arc;

const SUBJECT: &str = "scv1:conn:abc";

/// The access key `lookup` would serve, or `None` if it wants an exchange.
fn served(role_arn: &str, subject: &str) -> Option<String> {
    match lookup(role_arn, subject) {
        Action::Serve(creds) => Some(creds.access_key_id.clone()),
        Action::Exchange => None,
    }
}

fn wants_exchange(role_arn: &str, subject: &str) -> bool {
    served(role_arn, subject).is_none()
}

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
fn serves_credentials_that_are_comfortably_unexpired() {
    store("arn:fresh", SUBJECT, creds(3600));
    assert_eq!(
        served("arn:fresh", SUBJECT),
        Some("ASIAEXAMPLE".to_string())
    );
}

#[test]
fn exchanges_for_an_unknown_role() {
    assert!(wants_exchange("arn:never-stored", SUBJECT));
}

/// An expired credential must never be served, even though a renewal is
/// recorded as under way — there is nothing safe left to hand out.
#[test]
fn never_serves_an_expired_credential() {
    store("arn:expired", SUBJECT, creds(-1));
    assert!(wants_exchange("arn:expired", SUBJECT));
    assert!(wants_exchange("arn:expired", SUBJECT));
}

// ── renewal single-flight ──────────────────────────────────────────

/// Inside the refresh lead the credential is still valid, so exactly one caller
/// takes the exchange and everyone else keeps serving what they have. Without
/// this, a busy isolate renews with a burst of simultaneous STS calls.
#[test]
fn only_the_first_caller_in_the_refresh_lead_exchanges() {
    store("arn:renewing", SUBJECT, creds(30));

    assert!(
        wants_exchange("arn:renewing", SUBJECT),
        "first caller claims the renewal"
    );
    for _ in 0..10 {
        assert_eq!(
            served("arn:renewing", SUBJECT),
            Some("ASIAEXAMPLE".to_string()),
            "later callers serve the still-valid credential instead of piling on"
        );
    }
}

/// A renewed credential is served straight from the cache again, and the
/// renewal slot is released so the *next* lead window gets its own single flight.
#[test]
fn storing_a_renewal_releases_the_slot() {
    store("arn:renewed", SUBJECT, creds(30));
    assert!(wants_exchange("arn:renewed", SUBJECT));

    store("arn:renewed", SUBJECT, creds(3600));
    assert!(served("arn:renewed", SUBJECT).is_some());

    // Age back into the lead: a caller must be able to claim it again.
    store("arn:renewed", SUBJECT, creds(30));
    assert!(wants_exchange("arn:renewed", SUBJECT));
}

// ── cache key identity ─────────────────────────────────────────────

/// The role's trust policy conditions on the assertion's `sub`, so a second
/// connection pointing at the same role must not be served the first one's
/// credentials — that would succeed where STS would have refused.
#[test]
fn does_not_share_credentials_between_subjects_on_one_role() {
    store("arn:shared-role", "scv1:conn:allowed", creds(3600));
    assert!(served("arn:shared-role", "scv1:conn:allowed").is_some());
    assert!(wants_exchange("arn:shared-role", "scv1:conn:other"));
}

#[test]
fn does_not_share_credentials_between_roles_for_one_subject() {
    store("arn:role-a", SUBJECT, creds(3600));
    assert!(wants_exchange("arn:role-b", SUBJECT));
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
