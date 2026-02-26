//! OIDC/STS authentication for the S3 proxy gateway.
//!
//! This crate implements the `AssumeRoleWithWebIdentity` STS API, allowing
//! workloads like GitHub Actions to exchange OIDC tokens for temporary S3
//! credentials scoped to specific buckets and prefixes.
//!
//! # Flow
//!
//! 1. Client obtains a JWT from their OIDC provider (e.g., GitHub Actions ID token)
//! 2. Client calls `AssumeRoleWithWebIdentity` with the JWT and desired role
//! 3. This crate validates the JWT against the OIDC provider's JWKS
//! 4. Checks trust policy (issuer, audience, subject conditions)
//! 5. Mints temporary credentials (AccessKeyId/SecretAccessKey/SessionToken)
//! 6. Returns credentials to the client
//!
//! The client then uses these credentials to sign S3 requests normally.

pub mod jwks;
pub mod request;
pub mod responses;
pub mod sts;

use base64::Engine;
pub use jwks::JwksCache;
pub use request::try_parse_sts_request;
use request::StsRequest;
pub use responses::{build_sts_error_response, build_sts_response};
use s3_proxy_core::config::ConfigProvider;
use s3_proxy_core::error::ProxyError;
use s3_proxy_core::types::TemporaryCredentials;

/// Try to handle an STS request. Returns `Some((status, xml))` if the query
/// contained an STS action, or `None` if it wasn't an STS request.
pub async fn try_handle_sts<C: ConfigProvider>(
    query: Option<&str>,
    config: &C,
    jwks_cache: &JwksCache,
) -> Option<(u16, String)> {
    let sts_result = try_parse_sts_request(query)?;
    let (status, xml) = match sts_result {
        Ok(sts_request) => {
            match assume_role_with_web_identity(config, &sts_request, "STSPRXY", jwks_cache).await {
                Ok(creds) => build_sts_response(&creds),
                Err(e) => {
                    tracing::warn!(error = %e, "STS request failed");
                    build_sts_error_response(&e)
                }
            }
        }
        Err(e) => build_sts_error_response(&e),
    };
    Some((status, xml))
}

/// Decode JWT header and claims without signature verification.
fn jwt_decode_unverified(
    token: &str,
) -> Result<(serde_json::Value, serde_json::Value), ProxyError> {
    let mut parts = token.splitn(3, '.');
    let header_b64 = parts
        .next()
        .ok_or_else(|| ProxyError::InvalidOidcToken("malformed JWT".into()))?;
    let payload_b64 = parts
        .next()
        .ok_or_else(|| ProxyError::InvalidOidcToken("malformed JWT".into()))?;

    let decode = |s: &str| -> Result<serde_json::Value, ProxyError> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s)
            .map_err(|e| ProxyError::InvalidOidcToken(format!("base64url decode error: {}", e)))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| ProxyError::InvalidOidcToken(format!("invalid JWT JSON: {}", e)))
    };

    Ok((decode(header_b64)?, decode(payload_b64)?))
}

/// Validate an OIDC token and mint temporary credentials.
pub async fn assume_role_with_web_identity<C: ConfigProvider>(
    config: &C,
    sts_request: &StsRequest,
    key_prefix: &str,
    jwks_cache: &JwksCache,
) -> Result<TemporaryCredentials, ProxyError> {
    // Look up the role
    let role = config
        .get_role(&sts_request.role_arn)
        .await?
        .ok_or_else(|| ProxyError::RoleNotFound(sts_request.role_arn.to_string()))?;

    // Decode the JWT header and claims without verification to extract issuer and kid
    let (header, insecure_claims) = jwt_decode_unverified(&sts_request.web_identity_token)?;

    let issuer = insecure_claims
        .get("iss")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProxyError::InvalidOidcToken("missing iss claim".into()))?;

    // Verify the issuer is trusted
    if !role.trusted_oidc_issuers.iter().any(|i| i == issuer) {
        return Err(ProxyError::InvalidOidcToken(format!(
            "untrusted issuer: {}",
            issuer
        )));
    }

    // Fail fast on unsupported algorithms before making any network requests
    let alg = header
        .get("alg")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if alg != "RS256" {
        return Err(ProxyError::InvalidOidcToken(format!(
            "unsupported JWT algorithm: {}",
            alg
        )));
    }

    // Fetch JWKS (using cache) and verify the token
    let jwks = jwks_cache.get_or_fetch(issuer).await?;
    let kid = header
        .get("kid")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProxyError::InvalidOidcToken("JWT missing kid".into()))?;

    let key = jwks::find_key(&jwks, kid)?;
    let claims = jwks::verify_token(&sts_request.web_identity_token, key, issuer, &role)?;

    // Check subject conditions
    let subject = claims.get("sub").and_then(|v| v.as_str()).unwrap_or("");

    if !role.subject_conditions.is_empty() {
        let matches = role
            .subject_conditions
            .iter()
            .any(|pattern| subject_matches(subject, pattern));
        if !matches {
            return Err(ProxyError::InvalidOidcToken(format!(
                "subject '{}' does not match any conditions",
                subject
            )));
        }
    }

    // Mint temporary credentials (AWS enforces 900s minimum)
    const MIN_SESSION_DURATION_SECS: u64 = 900;
    let duration = sts_request
        .duration_seconds
        .unwrap_or(3600)
        .clamp(MIN_SESSION_DURATION_SECS, role.max_session_duration_secs);

    let creds = sts::mint_temporary_credentials(&role, subject, duration, key_prefix, &claims);

    // Store them
    config.store_temporary_credential(&creds).await?;

    Ok(creds)
}

/// Simple glob-style matching for subject conditions.
/// Supports `*` as a wildcard for any sequence of characters.
fn subject_matches(subject: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return subject == pattern;
    }

    let mut remaining = subject;

    // First part must be a prefix
    if !parts[0].is_empty() {
        if !remaining.starts_with(parts[0]) {
            return false;
        }
        remaining = &remaining[parts[0].len()..];
    }

    // Middle parts must appear in order
    for part in &parts[1..parts.len() - 1] {
        if part.is_empty() {
            continue;
        }
        match remaining.find(part) {
            Some(idx) => remaining = &remaining[idx + part.len()..],
            None => return false,
        }
    }

    // Last part must be a suffix
    let last = parts.last().unwrap();
    if !last.is_empty() {
        return remaining.ends_with(last);
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subject_matching() {
        assert!(subject_matches(
            "repo:org/repo:ref:refs/heads/main",
            "repo:org/repo:*"
        ));
        assert!(subject_matches("repo:org/repo:ref:refs/heads/main", "*"));
        assert!(subject_matches(
            "repo:org/repo:ref:refs/heads/main",
            "repo:org/repo:ref:refs/heads/main"
        ));
        assert!(!subject_matches(
            "repo:org/repo:ref:refs/heads/main",
            "repo:other/*"
        ));
        assert!(subject_matches(
            "repo:org/repo:ref:refs/heads/main",
            "repo:org/*:ref:refs/heads/*"
        ));
    }
}
