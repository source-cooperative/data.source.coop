//! Write-action classification. Kept wasm-free so it can be unit-tested
//! natively (see `tests/authz.rs`), despite the crate's `[lib] test = false`.
//! The rest of the write gate (read-only / signable / permission checks) is
//! trivial enough to live inline in the registry.

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
