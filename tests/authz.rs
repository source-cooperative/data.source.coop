//! Native unit tests for the wasm-free `authz` module, included via `#[path]`
//! (the lib itself is `cdylib` with `test = false`). Mirrors the pattern in
//! `tests/backend_auth.rs`.

#[path = "../src/authz.rs"]
mod authz;

use authz::{connection_accepts_writes, is_write_action, permits_write};
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

// ── connection_accepts_writes ──────────────────────────────────────

#[test]
fn connection_accepts_writes_when_writable_and_signable() {
    assert!(connection_accepts_writes(false, true));
}

#[test]
fn connection_rejects_read_only() {
    assert!(!connection_accepts_writes(true, true));
}

#[test]
fn connection_rejects_unsignable() {
    assert!(!connection_accepts_writes(false, false));
}

// ── permits_write ──────────────────────────────────────────────────

#[test]
fn permits_write_only_when_write_present() {
    assert!(permits_write(&perms(&["read", "write"])));
    assert!(permits_write(&perms(&["write"])));
    assert!(!permits_write(&perms(&["read"])));
    assert!(!permits_write(&perms(&[])));
}
