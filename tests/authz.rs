//! Native unit tests for the wasm-free `authz` module, included via `#[path]`
//! (the lib itself is `cdylib` with `test = false`). Mirrors the pattern in
//! `tests/backend_auth.rs`.
//!
//! `authz` references `crate::backend_auth`, so that module is pulled in here too
//! (under the test crate root) so the `crate::` path resolves the same way it
//! does in the lib build.

#[path = "../src/authz.rs"]
mod authz;
#[path = "../src/backend_auth.rs"]
mod backend_auth;

use authz::{decide_backend_auth, is_write_action};
use backend_auth::BackendAuth;
use multistore::error::ProxyError;
use multistore::types::Action;
use std::collections::HashMap;

#[test]
fn reads_are_not_writes() {
    assert!(!is_write_action(Action::GetObject));
    assert!(!is_write_action(Action::HeadObject));
    assert!(!is_write_action(Action::ListBucket));
}

#[test]
fn mutations_are_writes() {
    for action in [
        Action::PutObject,
        Action::DeleteObject,
        Action::CreateMultipartUpload,
        Action::UploadPart,
        Action::CompleteMultipartUpload,
        Action::AbortMultipartUpload,
    ] {
        assert!(is_write_action(action), "{action:?} should be a write");
    }
}

// ── decide_backend_auth: authorization → federation ordering (#142) ─────────
//
// The invariant under test: an unauthorized request must be denied *before* any
// backend authentication is applied — so a denial returns `AccessDenied` and
// leaves `options` empty (no `oidc_role_arn` / `skip_signature` leaked). If
// someone reordered the gate so federation ran before the checks, these tests
// would catch populated options on a denial.

fn role() -> BackendAuth {
    BackendAuth::S3WebIdentityRole {
        role_arn: "arn:aws:iam::1:role/r".into(),
    }
}

/// `None` authentication = the upstream subject-scoped fetch denied the caller.
/// Federation must never happen and `options` must stay empty, for reads or
/// writes alike.
#[test]
fn unauthorized_outcome_never_federates() {
    for is_write in [false, true] {
        let mut o = HashMap::new();
        let result = decide_backend_auth(
            None,
            false,
            is_write,
            true,
            &["write".to_string()],
            "conn-1",
            "s3",
            &mut o,
        );
        assert!(matches!(result, Err(ProxyError::AccessDenied)));
        assert!(o.is_empty(), "denied request must not emit backend options");
    }
}

#[test]
fn authorized_read_unsigned_populates_options() {
    let mut o = HashMap::new();
    decide_backend_auth(
        Some(&BackendAuth::Unsigned),
        false,
        false,
        false,
        &[],
        "conn-1",
        "s3",
        &mut o,
    )
    .unwrap();
    assert_eq!(o.get("skip_signature").map(String::as_str), Some("true"));
}

#[test]
fn authorized_read_federated_populates_options() {
    let mut o = HashMap::new();
    decide_backend_auth(
        Some(&role()),
        false,
        false,
        false,
        &[],
        "conn-1",
        "s3",
        &mut o,
    )
    .unwrap();
    assert_eq!(
        o.get("oidc_role_arn").map(String::as_str),
        Some("arn:aws:iam::1:role/r")
    );
    assert!(!o.contains_key("skip_signature"));
}

#[test]
fn write_by_anonymous_denied() {
    let mut o = HashMap::new();
    let result = decide_backend_auth(
        Some(&role()),
        false,
        true,
        false, // no subject
        &["write".to_string()],
        "conn-1",
        "s3",
        &mut o,
    );
    assert!(matches!(result, Err(ProxyError::AccessDenied)));
    assert!(o.is_empty());
}

#[test]
fn write_to_read_only_denied() {
    let mut o = HashMap::new();
    let result = decide_backend_auth(
        Some(&role()),
        true, // read_only
        true,
        true,
        &["write".to_string()],
        "conn-1",
        "s3",
        &mut o,
    );
    assert!(matches!(result, Err(ProxyError::AccessDenied)));
    assert!(o.is_empty());
}

#[test]
fn write_to_non_signable_denied() {
    // Unsigned (public) connections can't sign writes, even with the permission.
    let mut o = HashMap::new();
    let result = decide_backend_auth(
        Some(&BackendAuth::Unsigned),
        false,
        true,
        true,
        &["write".to_string()],
        "conn-1",
        "s3",
        &mut o,
    );
    assert!(matches!(result, Err(ProxyError::AccessDenied)));
    assert!(o.is_empty());
}

#[test]
fn write_without_write_permission_denied() {
    let mut o = HashMap::new();
    let result = decide_backend_auth(
        Some(&role()),
        false,
        true,
        true,
        &["read".to_string()], // no "write"
        "conn-1",
        "s3",
        &mut o,
    );
    assert!(matches!(result, Err(ProxyError::AccessDenied)));
    assert!(o.is_empty());
}

#[test]
fn authorized_write_populates_options() {
    let mut o = HashMap::new();
    decide_backend_auth(
        Some(&role()),
        false,
        true,
        true,
        &["read".to_string(), "WRITE".to_string()], // case-insensitive match
        "conn-1",
        "s3",
        &mut o,
    )
    .unwrap();
    assert_eq!(
        o.get("oidc_role_arn").map(String::as_str),
        Some("arn:aws:iam::1:role/r")
    );
    assert!(!o.contains_key("skip_signature"));
}
