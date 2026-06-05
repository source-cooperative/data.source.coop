//! Native unit tests for the wasm-free `backend_auth` module, included via
//! `#[path]` (the lib itself is `cdylib` with `test = false`). Mirrors the
//! pattern in `tests/pagination.rs`.

#[path = "../src/backend_auth.rs"]
mod backend_auth;

use backend_auth::{apply_backend_auth, BackendAuth};
use std::collections::HashMap;

// ── deserialization ────────────────────────────────────────────────

#[test]
fn deserializes_unsigned() {
    let a: BackendAuth = serde_json::from_str(r#"{"type":"unsigned"}"#).unwrap();
    assert_eq!(a, BackendAuth::Unsigned);
}

#[test]
fn deserializes_web_identity_role() {
    let a: BackendAuth = serde_json::from_str(
        r#"{"type":"s3_web_identity_role","role_arn":"arn:aws:iam::1:role/r"}"#,
    )
    .unwrap();
    assert_eq!(
        a,
        BackendAuth::S3WebIdentityRole {
            role_arn: "arn:aws:iam::1:role/r".into()
        }
    );
}

#[test]
fn unknown_type_deserializes_to_unsupported() {
    // The app-side GCP/Azure variants this proxy build doesn't implement must not
    // fail deserialization — `#[serde(other)]` catches them.
    let a: BackendAuth = serde_json::from_str(
        r#"{"type":"gcp_workload_identity","workload_identity_provider":"x","service_account":"y"}"#,
    )
    .unwrap();
    assert_eq!(a, BackendAuth::Unsupported);
}

// ── option translation ─────────────────────────────────────────────

#[test]
fn unsigned_sets_skip_signature() {
    let mut o = HashMap::new();
    apply_backend_auth(&BackendAuth::Unsigned, "conn-1", &mut o);
    assert_eq!(o.get("skip_signature").map(String::as_str), Some("true"));
    assert!(!o.contains_key("auth_type"));
}

#[test]
fn web_identity_role_sets_oidc_options_and_keeps_signing() {
    let mut o = HashMap::new();
    apply_backend_auth(
        &BackendAuth::S3WebIdentityRole {
            role_arn: "arn:aws:iam::1:role/r".into(),
        },
        "conn-1",
        &mut o,
    );
    assert_eq!(o.get("auth_type").map(String::as_str), Some("oidc"));
    assert_eq!(
        o.get("oidc_role_arn").map(String::as_str),
        Some("arn:aws:iam::1:role/r")
    );
    assert_eq!(
        o.get("oidc_subject").map(String::as_str),
        Some("scv1:conn:conn-1")
    );
    // Signing must stay ON for the federated path.
    assert!(!o.contains_key("skip_signature"));
}

#[test]
fn unsupported_serves_unsigned() {
    let mut o = HashMap::new();
    apply_backend_auth(&BackendAuth::Unsupported, "conn-1", &mut o);
    assert_eq!(o.get("skip_signature").map(String::as_str), Some("true"));
}
