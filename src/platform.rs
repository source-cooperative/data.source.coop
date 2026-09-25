//! Platform identity providers at `/.sts` (ADR-009, ADR-014): GitHub Actions
//! and the like, whose tokens say which workload is calling but not which
//! account it may act as. The account is the one `RoleArn` names, and only if
//! that account trusts the token's issuer and subject, which the Source API
//! answers. This module is the wasm-free half: which issuers are platform
//! issuers, reading a token before it is verified, and verifying it.

use std::collections::HashMap;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use multistore::error::ProxyError;
use multistore::types::RoleConfig;
use multistore_sts::jwks::{find_key, verify_token};
use multistore_sts::JwksCache;
use serde_json::Value;

/// The platform issuers in `PLATFORM_ISSUERS`, a JSON object from each issuer
/// URL to the audiences its tokens must carry. The audiences are per issuer so
/// that one issuer's audience never admits another's token (ADR-009). An
/// issuer with no audience is left out, as the person issuer is disabled
/// without one: a token minted for any other service could be exchanged here.
/// A value that does not parse trusts no platform issuer.
pub fn parse_issuers(json: &str) -> HashMap<String, Vec<String>> {
    let issuers: HashMap<String, Vec<String>> = match serde_json::from_str(json) {
        Ok(issuers) => issuers,
        Err(e) => {
            tracing::error!(
                "PLATFORM_ISSUERS is not an object of issuer to audiences ({e}); trusting none"
            );
            return HashMap::new();
        }
    };
    issuers
        .into_iter()
        .filter(|(issuer, audiences)| {
            if audiences.is_empty() {
                tracing::error!(%issuer, "platform issuer has no audience; refusing its tokens");
            }
            !audiences.is_empty()
        })
        .collect()
}

/// A token's header and claims, read without verifying anything: enough to
/// route it to its issuer and find the key it names. `None` if it is not a
/// JWT, an API key for one.
pub fn unverified(token: &str) -> Option<(Value, Value)> {
    let mut segments = token.split('.').map(|segment| {
        let json = URL_SAFE_NO_PAD.decode(segment).ok()?;
        serde_json::from_slice::<Value>(&json).ok()
    });
    Some((segments.next()??, segments.next()??))
}

/// Verify a platform issuer's token as the STS route verifies the person
/// issuer's (signature against the issuer's published keys, issuer, the
/// audiences `role` requires, `exp` and `nbf`) and return its subject.
pub async fn verify(
    token: &str,
    header: &Value,
    issuer: &str,
    role: &RoleConfig,
    jwks: &JwksCache,
) -> Result<String, ProxyError> {
    let kid = header
        .get("kid")
        .and_then(Value::as_str)
        .ok_or_else(|| ProxyError::InvalidOidcToken("JWT missing kid".into()))?;
    let keys = jwks.get_or_fetch(issuer).await?;
    let claims = verify_token(token, find_key(&keys, kid)?, issuer, role)?;
    subject(&claims).map(str::to_string)
}

/// The subject of verified `claims`, which must also carry an expiry:
/// multistore checks `exp` only when it is present, and a third-party token
/// with none would be replayable for good (ADR-004).
pub fn subject(claims: &Value) -> Result<&str, ProxyError> {
    if claims.get("exp").and_then(Value::as_i64).is_none() {
        return Err(ProxyError::InvalidOidcToken(
            "token has no exp claim".into(),
        ));
    }
    claims
        .get("sub")
        .and_then(Value::as_str)
        .filter(|sub| !sub.is_empty())
        .ok_or_else(|| ProxyError::InvalidOidcToken("token has no sub claim".into()))
}
