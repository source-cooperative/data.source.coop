//! Authentication and authorization.
//!
//! Handles:
//! - SigV4 request verification (incoming requests from clients)
//! - Identity resolution (mapping access key → principal)
//! - Authorization (checking if an identity can perform an operation)

use crate::config::ConfigProvider;
use crate::error::ProxyError;
use crate::types::{Action, ResolvedIdentity, S3Operation};
use hmac::{Hmac, Mac};
use http::HeaderMap;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Parsed SigV4 Authorization header.
#[derive(Debug, Clone)]
pub struct SigV4Auth {
    pub access_key_id: String,
    pub date_stamp: String,
    pub region: String,
    pub service: String,
    pub signed_headers: Vec<String>,
    pub signature: String,
}

/// Parse a SigV4 Authorization header.
///
/// Format: `AWS4-HMAC-SHA256 Credential=AKID/20240101/us-east-1/s3/aws4_request,
///           SignedHeaders=host;x-amz-date, Signature=abcdef...`
pub fn parse_sigv4_auth(auth_header: &str) -> Result<SigV4Auth, ProxyError> {
    let auth_header = auth_header
        .strip_prefix("AWS4-HMAC-SHA256 ")
        .ok_or_else(|| ProxyError::InvalidRequest("invalid auth scheme".into()))?;

    let mut credential = None;
    let mut signed_headers = None;
    let mut signature = None;

    for part in auth_header.split(", ") {
        if let Some(val) = part.strip_prefix("Credential=") {
            credential = Some(val);
        } else if let Some(val) = part.strip_prefix("SignedHeaders=") {
            signed_headers = Some(val);
        } else if let Some(val) = part.strip_prefix("Signature=") {
            signature = Some(val);
        }
    }

    let credential =
        credential.ok_or_else(|| ProxyError::InvalidRequest("missing Credential".into()))?;
    let signed_headers =
        signed_headers.ok_or_else(|| ProxyError::InvalidRequest("missing SignedHeaders".into()))?;
    let signature =
        signature.ok_or_else(|| ProxyError::InvalidRequest("missing Signature".into()))?;

    // Parse credential: AKID/date/region/service/aws4_request
    let cred_parts: Vec<&str> = credential.split('/').collect();
    if cred_parts.len() != 5 || cred_parts[4] != "aws4_request" {
        return Err(ProxyError::InvalidRequest(
            "malformed credential scope".into(),
        ));
    }

    Ok(SigV4Auth {
        access_key_id: cred_parts[0].to_string(),
        date_stamp: cred_parts[1].to_string(),
        region: cred_parts[2].to_string(),
        service: cred_parts[3].to_string(),
        signed_headers: signed_headers.split(';').map(String::from).collect(),
        signature: signature.to_string(),
    })
}

/// Verify a SigV4 signature against a known secret key.
pub fn verify_sigv4_signature(
    method: &http::Method,
    uri_path: &str,
    query_string: &str,
    headers: &HeaderMap,
    auth: &SigV4Auth,
    secret_access_key: &str,
    payload_hash: &str,
) -> Result<bool, ProxyError> {
    // Reconstruct canonical request
    let canonical_headers: String = auth
        .signed_headers
        .iter()
        .map(|name| {
            let value = headers
                .get(name.as_str())
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .trim();
            format!("{}:{}\n", name, value)
        })
        .collect();

    let signed_headers_str = auth.signed_headers.join(";");

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method, uri_path, query_string, canonical_headers, signed_headers_str, payload_hash
    );

    let canonical_request_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));

    let credential_scope = format!(
        "{}/{}/{}/aws4_request",
        auth.date_stamp, auth.region, auth.service
    );

    let amz_date = headers
        .get("x-amz-date")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        amz_date, credential_scope, canonical_request_hash
    );

    // Derive signing key
    let k_date = hmac_sha256(
        format!("AWS4{}", secret_access_key).as_bytes(),
        auth.date_stamp.as_bytes(),
    )?;
    let k_region = hmac_sha256(&k_date, auth.region.as_bytes())?;
    let k_service = hmac_sha256(&k_region, auth.service.as_bytes())?;
    let signing_key = hmac_sha256(&k_service, b"aws4_request")?;

    let expected_signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes())?);

    // Constant-time comparison
    Ok(constant_time_eq(
        expected_signature.as_bytes(),
        auth.signature.as_bytes(),
    ))
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<Vec<u8>, ProxyError> {
    let mut mac =
        HmacSha256::new_from_slice(key).map_err(|e| ProxyError::Internal(e.to_string()))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Resolve the identity of an incoming request.
///
/// Checks the Authorization header and resolves it against the config provider.
pub async fn resolve_identity<C: ConfigProvider>(
    headers: &HeaderMap,
    config: &C,
) -> Result<ResolvedIdentity, ProxyError> {
    let auth_header = match headers.get("authorization").and_then(|v| v.to_str().ok()) {
        Some(h) => h,
        None => return Ok(ResolvedIdentity::Anonymous),
    };

    let sig = parse_sigv4_auth(auth_header)?;

    // Check for temporary credentials first (session token present)
    if headers.get("x-amz-security-token").is_some() {
        if let Some(temp_cred) = config.get_temporary_credential(&sig.access_key_id).await? {
            // Verify session token matches
            let session_token = headers
                .get("x-amz-security-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if session_token != temp_cred.session_token {
                return Err(ProxyError::AccessDenied);
            }
            return Ok(ResolvedIdentity::Temporary {
                credentials: temp_cred,
            });
        }
        return Err(ProxyError::ExpiredCredentials);
    }

    // Check long-lived credentials
    if let Some(cred) = config.get_credential(&sig.access_key_id).await? {
        if !cred.enabled {
            return Err(ProxyError::AccessDenied);
        }
        if let Some(expires) = cred.expires_at {
            if expires <= chrono::Utc::now() {
                return Err(ProxyError::ExpiredCredentials);
            }
        }
        return Ok(ResolvedIdentity::LongLived { credential: cred });
    }

    Err(ProxyError::AccessDenied)
}

/// Check if a resolved identity is authorized to perform an operation.
pub fn authorize(
    identity: &ResolvedIdentity,
    operation: &S3Operation,
    bucket_config: &crate::types::BucketConfig,
) -> Result<(), ProxyError> {
    // Anonymous access check
    if matches!(identity, ResolvedIdentity::Anonymous) {
        if bucket_config.anonymous_access {
            // Anonymous users can only read
            let action = operation.action();
            if matches!(
                action,
                Action::GetObject | Action::HeadObject | Action::ListBucket
            ) {
                return Ok(());
            }
        }
        return Err(ProxyError::AccessDenied);
    }

    let scopes = match identity {
        ResolvedIdentity::Anonymous => unreachable!(),
        ResolvedIdentity::LongLived { credential } => &credential.allowed_scopes,
        ResolvedIdentity::Temporary { credentials } => &credentials.allowed_scopes,
    };

    let action = operation.action();
    let bucket = operation.bucket().unwrap_or_default().to_string();
    let key = match operation {
        S3Operation::ListBucket { raw_query, .. } => {
            // Extract prefix from raw query for authorization checks
            raw_query
                .as_deref()
                .and_then(|q| {
                    url::form_urlencoded::parse(q.as_bytes())
                        .find(|(k, _)| k == "prefix")
                        .map(|(_, v)| v.to_string())
                })
                .unwrap_or_default()
        }
        _ => operation.key().to_string(),
    };

    // Check if any scope grants access
    let authorized = scopes.iter().any(|scope| {
        if scope.bucket != bucket {
            return false;
        }
        if !scope.actions.contains(&action) {
            return false;
        }
        // Check prefix restrictions
        if scope.prefixes.is_empty() {
            return true; // Full bucket access
        }
        scope.prefixes.iter().any(|prefix| key.starts_with(prefix))
    });

    if authorized {
        Ok(())
    } else {
        Err(ProxyError::AccessDenied)
    }
}
