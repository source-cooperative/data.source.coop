//! API keys (ADR-013): opaque secrets a service account presents at `/.sts`
//! in place of an OIDC token. Nothing here verifies a signature — there is
//! none. The key's standing lives in source.coop, keyed by the key's SHA-256,
//! and this module is the wasm-free half of the exchange: recognising a key,
//! hashing it, and sealing credentials for the account the API names.

use multistore::error::ProxyError;
use multistore::types::{RoleConfig, TemporaryCredentials};
use multistore_sts::sts::mint_temporary_credentials;
use multistore_sts::TokenKey;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Every key starts with this, followed by 32 random bytes in base64url:
/// a fixed 47 characters, the pattern secret scanners are given.
pub const API_KEY_PREFIX: &str = "sck_";
const API_KEY_LEN: usize = 47;

/// The token, trimmed, if it has exactly a key's shape; `None` for anything
/// else — a JWT, a truncated key, the wrong case — so the JWT path or a local
/// refusal takes it without a lookup. Whitespace is trimmed first because
/// every hand-made token file ends in a newline, and some SDKs send it.
pub fn parse_api_key(token: &str) -> Option<&str> {
    let key = token.trim();
    (key.len() == API_KEY_LEN
        && key.starts_with(API_KEY_PREFIX)
        && key[API_KEY_PREFIX.len()..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    .then_some(key)
}

/// Whether the token so much as looks like a key — the prefix alone. Used to
/// refuse a key sent where it would be logged, before checking anything else.
pub fn looks_like_api_key(token: &str) -> bool {
    token.trim_start().starts_with(API_KEY_PREFIX)
}

/// Hex SHA-256 of a key: the record's key in source.coop, and all the proxy
/// ever sends of it.
pub fn key_hash(key: &str) -> String {
    Sha256::digest(key.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The Source API's answer for a presented key
/// (`POST /api/v1/service-account-keys/exchanges`): whether it may be
/// exchanged and, if so, for whom. Unknown, revoked, expired and disabled all
/// come back inactive and unnamed, so nothing distinguishes them here.
#[derive(Debug, Clone, Deserialize)]
pub struct KeyStanding {
    pub active: bool,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub key_id: Option<String>,
}

/// Credentials for an account the API has vouched for, sealed the way the
/// STS route seals every session, for the duration the client asked for
/// within the role's cap. The floor and default are AWS's and multistore's.
pub fn credentials_for(
    role: &RoleConfig,
    account_id: &str,
    duration_seconds: Option<u64>,
    token_key: &TokenKey,
) -> Result<TemporaryCredentials, ProxyError> {
    let duration = duration_seconds
        .unwrap_or(3600)
        .clamp(900, role.max_session_duration_secs);
    let mut creds = mint_temporary_credentials(
        role,
        account_id,
        duration,
        "STSPRXY",
        &serde_json::json!({}),
    );
    creds.session_token = token_key.seal(&creds)?;
    Ok(creds)
}
