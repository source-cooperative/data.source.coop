//! Authorization for product backends: write-action classification and the
//! authorization → federation decision ([`decide_backend_auth`]). Kept wasm-free
//! so both can be unit-tested natively (see `tests/authz.rs`), despite the
//! crate's `[lib] test = false`.

use std::collections::HashMap;

use multistore::error::ProxyError;
use multistore::types::Action;

use crate::backend_auth::{apply_backend_auth, BackendAuth};

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

/// Authorize a resolved product's request and, only on success, translate the
/// connection's backend authentication into multistore `backend_options`. This
/// is the single authorization → federation seam: `resolve_product` performs the
/// I/O (subject-scoped Source API fetches) and hands the outcome here as plain
/// values, so the security-critical ordering can be unit-tested off-wasm.
///
/// `authentication` is `Some` only when the caller's subject-scoped connection
/// fetch succeeded. `None` models that upstream lookup having *denied* the
/// caller, so we deny here without federating — the confused-deputy guard.
///
/// A write additionally requires an authenticated caller (`subject_present`) who
/// holds the product's `write` permission, a connection that is not `read_only`,
/// and a connection the proxy can actually sign as (an S3 web-identity role).
///
/// On every denial, `options` is left untouched: an unauthorized request must
/// never have backend credentials or `skip_signature` emitted on its behalf.
/// Only on success does [`apply_backend_auth`] populate `options`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_backend_auth(
    authentication: Option<&BackendAuth>,
    read_only: bool,
    is_write: bool,
    subject_present: bool,
    permissions: &[String],
    connection_id: &str,
    backend_type: &str,
    options: &mut HashMap<String, String>,
) -> Result<(), ProxyError> {
    // Confused-deputy guard: no authorized connection ⇒ never federate.
    let auth = authentication.ok_or(ProxyError::AccessDenied)?;

    if is_write {
        // Anonymous callers can never write (and there is no subject to query
        // permissions with).
        if !subject_present {
            return Err(ProxyError::AccessDenied);
        }
        // A connection can sign writes only via an S3 web-identity role; an
        // unsigned/unsupported connection (or one flagged read-only) cannot
        // accept them regardless of the caller's permissions.
        let signable = matches!(auth, BackendAuth::S3WebIdentityRole { .. });
        if read_only || !signable {
            return Err(ProxyError::AccessDenied);
        }
        // The caller must hold the product's `write` permission.
        if !permissions.iter().any(|p| p.eq_ignore_ascii_case("write")) {
            return Err(ProxyError::AccessDenied);
        }
    }

    apply_backend_auth(auth, connection_id, backend_type, options)
}
