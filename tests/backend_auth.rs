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

// ── lenient field deserialization (one bad entry must not poison the list) ──

#[derive(serde::Deserialize)]
struct Wrapper {
    #[serde(default, deserialize_with = "backend_auth::deserialize_lenient")]
    auth: BackendAuth,
}

#[test]
fn lenient_absent_is_unsigned() {
    let w: Wrapper = serde_json::from_str("{}").unwrap();
    assert_eq!(w.auth, BackendAuth::Unsigned);
}

#[test]
fn lenient_null_is_unsigned() {
    let w: Wrapper = serde_json::from_str(r#"{"auth":null}"#).unwrap();
    assert_eq!(w.auth, BackendAuth::Unsigned);
}

#[test]
fn lenient_valid_role_parses() {
    let w: Wrapper = serde_json::from_str(
        r#"{"auth":{"type":"s3_web_identity_role","role_arn":"arn:aws:iam::1:role/r"}}"#,
    )
    .unwrap();
    assert_eq!(
        w.auth,
        BackendAuth::S3WebIdentityRole {
            role_arn: "arn:aws:iam::1:role/r".into()
        }
    );
}

#[test]
fn lenient_malformed_becomes_unsupported_not_error() {
    // Missing role_arn, a wrong-typed value, and an unknown type all degrade to
    // Unsupported instead of erroring — so they can't fail the whole list parse.
    for bad in [
        r#"{"auth":{"type":"s3_web_identity_role"}}"#,
        r#"{"auth":"garbage"}"#,
        r#"{"auth":{"type":"gcp_workload_identity","workload_identity_provider":"x"}}"#,
    ] {
        let w: Wrapper = serde_json::from_str(bad).unwrap();
        assert_eq!(w.auth, BackendAuth::Unsupported, "input: {bad}");
    }
}

// ── option translation ─────────────────────────────────────────────

#[test]
fn unsigned_sets_skip_signature() {
    let mut o = HashMap::new();
    apply_backend_auth(&BackendAuth::Unsigned, "conn-1", "s3", &mut o).unwrap();
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
        "s3",
        &mut o,
    )
    .unwrap();
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
fn unsupported_fails_closed() {
    let mut o = HashMap::new();
    let result = apply_backend_auth(&BackendAuth::Unsupported, "conn-1", "s3", &mut o);
    assert!(result.is_err());
    // Must not have set unsigned (or any) options as a side effect.
    assert!(o.is_empty());
}

#[test]
fn web_identity_role_on_non_s3_fails_closed() {
    let mut o = HashMap::new();
    let result = apply_backend_auth(
        &BackendAuth::S3WebIdentityRole {
            role_arn: "arn:aws:iam::1:role/r".into(),
        },
        "conn-1",
        "az",
        &mut o,
    );
    assert!(result.is_err());
    // No AWS/OIDC options must leak into the non-S3 backend's option map.
    assert!(o.is_empty());
}
