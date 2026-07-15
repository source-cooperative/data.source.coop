//! STS credential registry for token exchange.
//!
//! Provides a hardcoded `_default` role that trusts the Source Cooperative auth
//! provider, enabling clients to exchange OIDC tokens for temporary S3-style credentials.

use multistore::error::ProxyError;
use multistore::registry::CredentialRegistry;
use multistore::types::{RoleConfig, StoredCredential};

/// Credential registry that serves a single hardcoded `_default` role.
///
/// The default role trusts the Source Cooperative auth provider with no scope
/// restrictions, so any user holding a token for one of the configured
/// audiences (`required_audiences`) can obtain temporary credentials.
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
    ///
    /// `max_session_duration_secs` is the ceiling for client-requested
    /// `DurationSeconds`. Clients still get the multistore default (1h) unless
    /// they request more, up to this cap. These are self-minted sealed-token
    /// credentials with no revocation, so a longer TTL widens the leak window.
    pub fn new(
        oidc_issuer: String,
        required_audiences: Vec<String>,
        max_session_duration_secs: u64,
    ) -> Self {
        Self {
            default_role: RoleConfig {
                role_id: "_default".to_string(),
                name: "Default".to_string(),
                trusted_oidc_issuers: vec![oidc_issuer],
                required_audiences,
                subject_conditions: vec![],
                allowed_scopes: vec![], // unlimited
                max_session_duration_secs,
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
        if is_default_role(role_id) {
            Ok(Some(self.default_role.clone()))
        } else {
            Ok(None)
        }
    }
}

/// Whether `role_id` names the `_default` role — literally, or via an
/// ARN-shaped alias whose resource is `role/_default` (any partition/account,
/// e.g. `arn:aws:iam::000000000000:role/_default`).
///
/// The alias exists because AWS SDKs validate `RoleArn` client-side (ARN shape,
/// 20-character minimum) before the request is ever sent, so a bare `_default`
/// can't reach the server from standard tooling. Accepting the alias keeps
/// `/.sts` a drop-in `AssumeRoleWithWebIdentity` target for unmodified SDKs
/// (see source-cooperative/data.source.coop#184). Same role, same trust model —
/// only the name is longer; the partition/account portion is ignored rather
/// than validated because it carries no meaning here.
pub(crate) fn is_default_role(role_id: &str) -> bool {
    role_id == "_default" || (role_id.starts_with("arn:") && role_id.ends_with(":role/_default"))
}
