//! No-op credential registry for anonymous-only access.

use multistore::error::ProxyError;
use multistore::registry::CredentialRegistry;
use multistore::types::{RoleConfig, StoredCredential};

/// Credential registry that always returns `None`.
///
/// Used for the anonymous-only MVP where no authentication is supported.
#[derive(Clone)]
pub struct NoopCredentialRegistry;

impl CredentialRegistry for NoopCredentialRegistry {
    async fn get_credential(
        &self,
        _access_key_id: &str,
    ) -> Result<Option<StoredCredential>, ProxyError> {
        Ok(None)
    }

    async fn get_role(&self, _role_id: &str) -> Result<Option<RoleConfig>, ProxyError> {
        Ok(None)
    }
}
