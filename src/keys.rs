//! API keys (ADR-013, amended by ADR-014): long-lived JWTs the proxy signs
//! for a service account, handed out as `sck_` + JWT and exchanged at `/.sts`
//! like any other identity token — except that the proxy verifies them
//! against its own signing key in process, and asks the Source API whether
//! the key's `jti` is still active before it mints anything.
//!
//! Kept wasm-free so the minting and verification rules are unit-tested
//! natively (see `tests/keys.rs`), despite the crate's `[lib] test = false`.
//! The network halves — the standing lookup and the manager check — live in
//! `source_api::cache`, and the wiring in `lib.rs`.

use multistore::error::ProxyError;
use multistore::types::{RoleConfig, TemporaryCredentials};
use multistore_oidc_provider::jwt::JwtSigner;
use multistore_sts::jwks::{verify_token, JwkKey, JwksResponse};
use multistore_sts::sts::mint_temporary_credentials;
use multistore_sts::TokenKey;
use serde::Deserialize;

/// Every key starts with this. A JWT always begins `eyJ`, so a leaked key
/// matches `sck_eyJ[\w-]+\.[\w-]+\.[\w-]+` — the pattern secret scanners are
/// given. Stripped before verification; a bare JWT from this issuer is not a
/// key, and the STS route rejects it as an untrusted issuer.
pub const API_KEY_PREFIX: &str = "sck_";

/// The `type` claim that marks a token as an API key, telling it apart from
/// the assertions the proxy signs for outbound federation under the same key.
pub const API_KEY_TYPE: &str = "api_key";

/// What source.coop sends to `POST /.keys` for a key it has recorded.
#[derive(Debug, Deserialize)]
pub struct KeyRequest {
    pub account_id: String,
    pub jti: String,
    /// RFC 3339; `null` for a key that lasts until revoked.
    pub expires_at: Option<String>,
}

/// The Source API's answer for a presented key
/// (`POST /api/v1/service-account-keys/{jti}/exchanges`).
#[derive(Debug, Deserialize)]
pub struct KeyStanding {
    pub active: bool,
}

/// A verified key: who it is for, which record it is, and its claims.
#[derive(Debug)]
pub struct ApiKey {
    pub account_id: String,
    pub jti: String,
    pub claims: serde_json::Value,
}

/// The role an API key assumes: the proxy trusts its own issuer, for tokens
/// minted for itself, from any service account — and lets them omit `exp`,
/// because validity is the key record's, checked on every exchange.
pub fn api_key_role(issuer: &str, max_session_duration_secs: u64) -> RoleConfig {
    RoleConfig {
        role_id: "_default".to_string(),
        name: "API key".to_string(),
        trusted_oidc_issuers: vec![issuer.to_string()],
        required_audiences: vec![issuer.to_string()],
        subject_conditions: vec!["*".to_string()],
        allowed_scopes: vec![],
        max_session_duration_secs,
        allow_missing_exp_from: vec![issuer.to_string()],
    }
}

/// The claims of a key for `req`, dated `now` (unix seconds).
pub fn api_key_claims(
    issuer: &str,
    req: &KeyRequest,
    now: i64,
) -> Result<serde_json::Value, ProxyError> {
    if req.account_id.is_empty() || req.jti.is_empty() {
        return Err(ProxyError::InvalidRequest(
            "account_id and jti are required".into(),
        ));
    }
    let mut claims = serde_json::json!({
        "iss": issuer,
        "sub": req.account_id,
        "aud": issuer,
        "jti": req.jti,
        "iat": now,
        "type": API_KEY_TYPE,
    });
    if let Some(at) = &req.expires_at {
        let exp = chrono::DateTime::parse_from_rfc3339(at)
            .map_err(|e| ProxyError::InvalidRequest(format!("expires_at: {e}")))?
            .timestamp();
        if exp <= now {
            return Err(ProxyError::InvalidRequest(
                "expires_at is in the past".into(),
            ));
        }
        claims["exp"] = exp.into();
    }
    Ok(claims)
}

/// Sign a key for `req`: the prefix, then the JWT.
pub fn mint_api_key(
    signer: &JwtSigner,
    issuer: &str,
    req: &KeyRequest,
    now: i64,
) -> Result<String, ProxyError> {
    let claims = api_key_claims(issuer, req, now)?;
    let jwt = signer
        .sign_claims(&claims)
        .map_err(|e| ProxyError::Internal(format!("sign API key: {e}")))?;
    Ok(format!("{API_KEY_PREFIX}{jwt}"))
}

/// The JWT inside a key, or `None` when `token` is not a key at all.
pub fn strip_api_key(token: &str) -> Option<&str> {
    token.strip_prefix(API_KEY_PREFIX)
}

/// The proxy's own signing keys as a JWK set — what a relying party would
/// fetch from `/.well-known/jwks.json`, built in process because a Worker
/// cannot fetch itself. Pass the previous key too during a rotation, so keys
/// signed before it still verify while that key is served.
pub fn own_jwks(signers: &[&JwtSigner]) -> JwksResponse {
    let keys: Vec<_> = signers.iter().map(|s| (s.public_key(), s.kid())).collect();
    serde_json::from_str(&multistore_oidc_provider::jwks::jwks_json(&keys))
        .expect("jwks_json is a JWKS")
}

/// Verify `token` — the JWT, prefix already stripped — with whichever of
/// `jwks` signed it, and check it is an API key: `type` says so, and `sub`
/// and `jti` are present.
pub fn verify_api_key(
    token: &str,
    jwks: &JwksResponse,
    issuer: &str,
    role: &RoleConfig,
) -> Result<ApiKey, ProxyError> {
    let claims = verify_with_any_key(token, &jwks.keys, issuer, role)?;
    if claims.get("type").and_then(|v| v.as_str()) != Some(API_KEY_TYPE) {
        return Err(ProxyError::InvalidOidcToken("not an API key".into()));
    }
    let field = |name: &str| {
        claims
            .get(name)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| ProxyError::InvalidOidcToken(format!("API key has no {name}")))
    };
    Ok(ApiKey {
        account_id: field("sub")?,
        jti: field("jti")?,
        claims,
    })
}

/// Verify `token` with whichever of `keys` signed it. A set holds a key or
/// two, so trying each costs less than decoding the header to pick one; the
/// last failure is the one reported.
pub fn verify_with_any_key(
    token: &str,
    keys: &[JwkKey],
    issuer: &str,
    role: &RoleConfig,
) -> Result<serde_json::Value, ProxyError> {
    let mut last = ProxyError::InvalidOidcToken("no signing keys".into());
    for key in keys {
        match verify_token(token, key, issuer, role) {
            Ok(claims) => return Ok(claims),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Credentials for a verified key, sealed the way the STS route seals every
/// session, for the duration the client asked for within the role's cap.
pub fn credentials_for(
    role: &RoleConfig,
    key: &ApiKey,
    duration_seconds: Option<u64>,
    token_key: &TokenKey,
) -> Result<TemporaryCredentials, ProxyError> {
    // The same floor and default as multistore's exchange (AWS's 900s minimum).
    let duration = duration_seconds
        .unwrap_or(3600)
        .clamp(900, role.max_session_duration_secs);
    let mut creds =
        mint_temporary_credentials(role, &key.account_id, duration, "STSPRXY", &key.claims)?;
    creds.session_token = token_key.seal(&creds)?;
    Ok(creds)
}
