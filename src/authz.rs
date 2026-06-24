//! Caller-side write authorization.
//!
//! Backend *signing* (how the proxy authenticates outbound) lives in
//! [`backend_auth`](crate::backend_auth); this module is the orthogonal
//! concern: whether a given *caller* may perform a *write* on a product. The
//! decisions are pure functions of facts the registry resolves from the Source
//! API, kept free of wasm-only deps so they can be unit-tested natively (see
//! `tests/authz.rs`), despite the crate's `[lib] test = false`.

use multistore::types::Action;

/// Whether an S3 action mutates the backend. Reads (GET/HEAD/LIST) are served
/// without a write check; everything else is a write and must be authorized.
///
/// A denylist over the closed [`Action`] set, so it is fail-safe by direction:
/// any action that is not explicitly a read is treated as a write. A future
/// read-only action added upstream would be (harmlessly) gated as a write until
/// added here, never the reverse.
pub(crate) fn is_write_action(action: Action) -> bool {
    !matches!(
        action,
        Action::GetObject | Action::HeadObject | Action::ListBucket
    )
}

/// Connection-level preconditions for a write, evaluable with no caller lookup:
/// the connection must not be read-only and must be able to sign requests (an
/// S3 web-identity role — an unsigned/public or unsupported connection has no
/// credentials to write with). Checked *before* the per-caller permission fetch,
/// so a write the connection can never accept is denied without an API call.
pub(crate) fn connection_accepts_writes(read_only: bool, signable: bool) -> bool {
    !read_only && signable
}

/// Whether the caller's product permissions (as reported by the Source API's
/// `/permissions` endpoint) include write access.
pub(crate) fn permits_write(permissions: &[String]) -> bool {
    permissions.iter().any(|p| p == "write")
}
