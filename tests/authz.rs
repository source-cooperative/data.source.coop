//! Native unit tests for the wasm-free `authz` module, included via `#[path]`
//! (the lib itself is `cdylib` with `test = false`). Mirrors the pattern in
//! `tests/backend_auth.rs`.

#[path = "../src/authz.rs"]
mod authz;

use authz::{authorize_write, is_write_action};
use multistore::types::Action;

fn perms(p: &[&str]) -> Vec<String> {
    p.iter().map(|s| s.to_string()).collect()
}

// ── is_write_action ────────────────────────────────────────────────

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

// ── authorize_write ────────────────────────────────────────────────

#[test]
fn allowed_when_writable_signable_and_permitted() {
    assert!(authorize_write(false, true, &perms(&["read", "write"])).is_ok());
}

#[test]
fn denied_on_read_only_connection() {
    assert!(authorize_write(true, true, &perms(&["read", "write"])).is_err());
}

#[test]
fn denied_when_connection_not_signable() {
    assert!(authorize_write(false, false, &perms(&["read", "write"])).is_err());
}

#[test]
fn denied_without_write_permission() {
    assert!(authorize_write(false, true, &perms(&["read"])).is_err());
    assert!(authorize_write(false, true, &perms(&[])).is_err());
}
