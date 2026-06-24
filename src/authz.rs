//! Caller-side write authorization.
//!
//! Backend *signing* (how the proxy authenticates outbound) lives in
//! [`backend_auth`](crate::backend_auth); this module is the orthogonal
//! concern: whether a given *caller* may perform a *write* on a product. The
//! decision is a pure function of facts the registry resolves from the Source
//! API, kept free of wasm-only deps so it can be unit-tested natively (see
//! `tests/authz.rs`), despite the crate's `[lib] test = false`.

use multistore::error::ProxyError;
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

/// Decide whether a write may proceed against a resolved product/connection.
///
/// Called only for write operations, and only once the caller is known to be
/// authenticated (anonymous writes are rejected by the registry before this,
/// where the caller's identity/subject is in hand) and the product + connection
/// have already been resolved against the subject-scoped Source API (so the
/// caller is cleared to *see* the resource). All remaining conditions must hold
/// or the write is denied with `AccessDenied`:
///
/// 1. the data connection is not read-only;
/// 2. the connection can sign writes (`signable`) — an unsigned/public or
///    unsupported connection has no credentials to write with, so a write would
///    only fail opaquely at the backend;
/// 3. the caller holds the `write` permission the Source API reports for the
///    product.
pub(crate) fn authorize_write(
    read_only: bool,
    signable: bool,
    permissions: &[String],
) -> Result<(), ProxyError> {
    if read_only {
        return Err(ProxyError::AccessDenied);
    }
    if !signable {
        return Err(ProxyError::AccessDenied);
    }
    if !permissions.iter().any(|p| p == "write") {
        return Err(ProxyError::AccessDenied);
    }
    Ok(())
}
