//! Native unit tests for the wasm-free `authz` module, included via `#[path]`
//! (the lib itself is `cdylib` with `test = false`). Mirrors the pattern in
//! `tests/backend_auth.rs`.

#[path = "../src/authz.rs"]
mod authz;

use authz::is_write_action;
use multistore::types::Action;

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
