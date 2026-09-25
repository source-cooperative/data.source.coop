//! STS credential registry for token exchange.
//!
//! Serves the hardcoded Roles (ADR-014): `FullAccess`, everything the caller's
//! memberships allow, and `ReadOnly`, the same with writing removed. `_default`
//! is `FullAccess` under the name existing clients already use. There is no
//! lookup: account-owned Roles (ADR-010) are deferred, and a Role only ever
//! subtracts from the account's own permissions, so any caller may name either.

use multistore::error::ProxyError;
use multistore::registry::CredentialRegistry;
use multistore::types::{AccessScope, Action, RoleConfig, StoredCredential};

/// The bucket a Role's scope names to cover every product. Only the proxy's
/// registry reads scopes — multistore's own scope check never runs on this
/// gateway — so the wildcard means what `authz::ceiling_permits` says it does.
pub(crate) const ALL_PRODUCTS: &str = "*";

/// Credential registry that serves the hardcoded Roles.
#[derive(Clone)]
pub struct StsCredentialRegistry {
    oidc_issuer: String,
    required_audiences: Vec<String>,
    max_session_duration_secs: u64,
}

impl StsCredentialRegistry {
    /// Create a new registry whose Roles trust the given auth issuer.
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
            oidc_issuer,
            required_audiences,
            max_session_duration_secs,
        }
    }
}

/// The Role `role_arn` names, trusting `oidc_issuer` for tokens minted for one
/// of `required_audiences`; `None` for a name the proxy does not serve. Never a
/// fallback: a workload that asks for a Role it cannot have fails at exchange
/// rather than receiving different access than it asked for. Shared with the
/// API-key exchange, which mints under the named Role once the API has named
/// the account (`keys::credentials_for`).
pub(crate) fn role(
    role_arn: &str,
    oidc_issuer: String,
    required_audiences: Vec<String>,
    max_session_duration_secs: u64,
) -> Option<RoleConfig> {
    let name = role_name(role_arn)?;
    let allowed_scopes = match name {
        // No scopes, no ceiling: the account's permissions are the only limit.
        "FullAccess" | "_default" => vec![],
        // Sealed into the session; `authz::ceiling_permits` enforces it.
        "ReadOnly" => vec![AccessScope {
            bucket: ALL_PRODUCTS.to_string(),
            prefixes: vec![],
            actions: vec![Action::GetObject, Action::HeadObject, Action::ListBucket],
        }],
        _ => return None,
    };
    Some(RoleConfig {
        role_id: name.to_string(),
        name: name.to_string(),
        trusted_oidc_issuers: vec![oidc_issuer],
        required_audiences,
        subject_conditions: vec![],
        allowed_scopes,
        max_session_duration_secs,
    })
}

/// The Role name in `role_arn`: a bare name, or the `role/<name>` resource of
/// an ARN of any partition and account, such as
/// `arn:aws:iam::000000000000:role/ReadOnly`.
///
/// The ARN form exists because AWS SDKs validate `RoleArn` client-side (ARN
/// shape, 20-character minimum) before the request is ever sent, so a bare name
/// can't reach the server from standard tooling (see
/// source-cooperative/data.source.coop#184). The partition and account carry no
/// meaning for the Role itself, so they are ignored rather than validated.
fn role_name(role_arn: &str) -> Option<&str> {
    if !role_arn.starts_with("arn:") {
        return Some(role_arn);
    }
    role_arn.splitn(6, ':').nth(5)?.strip_prefix("role/")
}

/// The account segment of an ARN-form `role_arn`
/// (`arn:aws:iam::<account>:role/<name>`): the account a platform IdP's token
/// asks to act as (ADR-014). `None` for a bare name or an empty account.
pub(crate) fn account(role_arn: &str) -> Option<&str> {
    match role_arn.splitn(6, ':').collect::<Vec<_>>()[..] {
        ["arn", _, _, _, account, _] if !account.is_empty() => Some(account),
        _ => None,
    }
}

/// Whether `id` is a service account's id, `{owner}--{name}`: source.coop's
/// `SERVICE_ACCOUNT_ID_REGEX` and its 82-character limit. Each half is at least
/// two of `a-z`, `0-9` and inner single hyphens, so the one `--` is the
/// separator, and no person's or organisation's handle, nor an Ory identity
/// id, can match.
pub(crate) fn is_service_account_id(id: &str) -> bool {
    let half = |part: &str| {
        part.len() >= 2
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !part.starts_with('-')
            && !part.ends_with('-')
            && !part.contains("--")
    };
    id.len() <= 82
        && id
            .split_once("--")
            .is_some_and(|(owner, name)| half(owner) && half(name))
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
        Ok(role(
            role_id,
            self.oidc_issuer.clone(),
            self.required_audiences.clone(),
            self.max_session_duration_secs,
        ))
    }
}
