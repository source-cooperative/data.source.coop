//! STS credential registry for token exchange.
//!
//! Provides a hardcoded `_default` role that trusts the Source Cooperative auth
//! provider, enabling clients to exchange OIDC tokens for temporary S3-style credentials.

use multistore::error::ProxyError;
use multistore::registry::CredentialRegistry;
use multistore::types::{RoleConfig, StoredCredential};

/// Credential registry that serves a single hardcoded `_default` role.
///
/// The default role trusts the Source Cooperative auth provider with no
/// scope restrictions, allowing any authenticated user to obtain temporary
/// credentials.
#[derive(Clone)]
pub struct StsCredentialRegistry {
    default_role: RoleConfig,
}

impl StsCredentialRegistry {
    /// Create a new registry whose `_default` role trusts the given auth issuer.
    ///
    /// `required_audiences` restricts token exchange to subject tokens minted
    /// for one of these OAuth clients (the `aud` claim); a token is accepted if
    /// it matches any. An empty list would let an ID token a user granted to any
    /// third-party client registered with the issuer be exchanged for that
    /// user's proxy credentials, so callers gate on a non-empty list.
    pub fn new(oidc_issuer: String, required_audiences: Vec<String>) -> Self {
        Self {
            default_role: RoleConfig {
                role_id: "_default".to_string(),
                name: "Default".to_string(),
                trusted_oidc_issuers: vec![oidc_issuer],
                required_audiences,
                subject_conditions: vec![],
                allowed_scopes: vec![], // unlimited
                max_session_duration_secs: 3600,
            },
        }
    }
}

impl CredentialRegistry for StsCredentialRegistry {
    async fn get_credential(
        &self,
        _access_key_id: &str,
    ) -> Result<Option<StoredCredential>, ProxyError> {
        // No long-lived credentials — all access is via STS token exchange.
        Ok(None)
    }

    async fn get_role(&self, role_id: &str) -> Result<Option<RoleConfig>, ProxyError> {
        // TODO: Eventually look up roles via the Source Cooperative API so that
        // individual repositories can define custom roles with fine-grained
        // scope and subject restrictions (e.g. per-repo CI/CD access).
        // For now, only the hardcoded `_default` role is supported.
        if role_id == "_default" {
            Ok(Some(self.default_role.clone()))
        } else {
            Ok(None)
        }
    }
}
