//! Native unit tests for the wasm-free `authz` module, included via `#[path]`
//! (the lib itself is `cdylib` with `test = false`). Mirrors the pattern in
//! `tests/backend_auth.rs`.
//!
//! `authz` references `crate::backend_auth` and `crate::sts`, so those modules
//! are pulled in here too (under the test crate root) so the `crate::` paths
//! resolve the same way they do in the lib build.

#[path = "../src/authz.rs"]
mod authz;
#[path = "../src/backend_auth.rs"]
mod backend_auth;
#[path = "../src/sts.rs"]
mod sts;

use authz::{ceiling_permits, decide_backend_auth, is_write_action};
use backend_auth::BackendAuth;
use multistore::error::ProxyError;
use multistore::types::{AccessScope, Action};
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

// ── ceiling_permits: the Role ceiling (ADR-011) ─────────────────────────────

const EVERY_ACTION: [Action; 10] = [
    Action::GetObject,
    Action::GetObjectVersion,
    Action::HeadObject,
    Action::PutObject,
    Action::ListBucket,
    Action::CreateMultipartUpload,
    Action::UploadPart,
    Action::CompleteMultipartUpload,
    Action::AbortMultipartUpload,
    Action::DeleteObject,
];

/// The scopes a Role seals into every session it mints.
fn sealed(role: &str) -> Vec<AccessScope> {
    sts::role(role, "https://auth.example.test".into(), vec![], 3600)
        .unwrap()
        .allowed_scopes
}

/// The ceiling and the write gate must agree on what a read is, or ReadOnly
/// either refuses a read or lets a write through.
#[test]
fn read_only_allows_exactly_the_reads() {
    let read_only = sealed("ReadOnly");
    for action in EVERY_ACTION {
        assert_eq!(
            ceiling_permits(&read_only, action),
            !is_write_action(action),
            "{action:?}"
        );
    }
}

#[test]
fn full_access_and_its_alias_have_no_ceiling() {
    for role in ["FullAccess", "_default"] {
        let scopes = sealed(role);
        for action in EVERY_ACTION {
            assert!(ceiling_permits(&scopes, action), "{role} {action:?}");
        }
    }
}

/// Nothing the proxy mints is narrower than every product, so a scope that is
/// must not be read as if it were.
#[test]
fn a_scope_narrower_than_every_product_permits_nothing() {
    for scope in [
        AccessScope {
            bucket: "acme:data".into(),
            prefixes: vec![],
            actions: vec![Action::GetObject],
        },
        AccessScope {
            bucket: "*".into(),
            prefixes: vec!["public/".into()],
            actions: vec![Action::GetObject],
        },
    ] {
        assert!(!ceiling_permits(&[scope], Action::GetObject));
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
