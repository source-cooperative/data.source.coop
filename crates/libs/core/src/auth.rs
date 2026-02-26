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
/// Parses the SigV4 Authorization header, looks up the credential, verifies
/// the signature, and returns the resolved identity.
pub async fn resolve_identity<C: ConfigProvider>(
    method: &http::Method,
    uri_path: &str,
    query_string: &str,
    headers: &HeaderMap,
    config: &C,
) -> Result<ResolvedIdentity, ProxyError> {
    let auth_header = match headers.get("authorization").and_then(|v| v.to_str().ok()) {
        Some(h) => h,
        None => return Ok(ResolvedIdentity::Anonymous),
    };

    let sig = parse_sigv4_auth(auth_header)?;

    // The payload hash is sent by the client in x-amz-content-sha256.
    // For streaming uploads this is the UNSIGNED-PAYLOAD or
    // STREAMING-AWS4-HMAC-SHA256-PAYLOAD sentinel — both are valid
    // canonical-request inputs per the SigV4 spec.
    let payload_hash = headers
        .get("x-amz-content-sha256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("UNSIGNED-PAYLOAD");

    // Check for temporary credentials first (session token present)
    if headers.get("x-amz-security-token").is_some() {
        if let Some(temp_cred) = config.get_temporary_credential(&sig.access_key_id).await? {
            // Verify session token matches (constant-time to avoid timing leaks)
            let session_token = headers
                .get("x-amz-security-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !constant_time_eq(session_token.as_bytes(), temp_cred.session_token.as_bytes()) {
                return Err(ProxyError::AccessDenied);
            }

            // Verify SigV4 signature
            if !verify_sigv4_signature(
                method,
                uri_path,
                query_string,
                headers,
                &sig,
                &temp_cred.secret_access_key,
                payload_hash,
            )? {
                return Err(ProxyError::SignatureDoesNotMatch);
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

        // Verify SigV4 signature
        if !verify_sigv4_signature(
            method,
            uri_path,
            query_string,
            headers,
            &sig,
            &cred.secret_access_key,
            payload_hash,
        )? {
            return Err(ProxyError::SignatureDoesNotMatch);
        }

        return Ok(ResolvedIdentity::LongLived { credential: cred });
    }

    Err(ProxyError::AccessDenied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AccessScope, Action, BucketConfig, RoleConfig, StoredCredential, TemporaryCredentials,
    };

    // ── Mock config provider ──────────────────────────────────────────

    #[derive(Clone)]
    struct MockConfig {
        credentials: Vec<StoredCredential>,
        temp_credentials: Vec<TemporaryCredentials>,
    }

    impl MockConfig {
        fn with_credential(secret: &str) -> Self {
            Self {
                credentials: vec![StoredCredential {
                    access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
                    secret_access_key: secret.into(),
                    principal_name: "test-user".into(),
                    allowed_scopes: vec![AccessScope {
                        bucket: "test-bucket".into(),
                        prefixes: vec![],
                        actions: vec![Action::GetObject],
                    }],
                    created_at: chrono::Utc::now(),
                    expires_at: None,
                    enabled: true,
                }],
                temp_credentials: vec![],
            }
        }

        fn with_temp_credential(secret: &str, session_token: &str) -> Self {
            Self {
                credentials: vec![],
                temp_credentials: vec![TemporaryCredentials {
                    access_key_id: "ASIATEMP1234EXAMPLE".into(),
                    secret_access_key: secret.into(),
                    session_token: session_token.into(),
                    expiration: chrono::Utc::now() + chrono::Duration::hours(1),
                    allowed_scopes: vec![AccessScope {
                        bucket: "test-bucket".into(),
                        prefixes: vec![],
                        actions: vec![Action::GetObject],
                    }],
                    assumed_role_id: "role-1".into(),
                    source_identity: "test".into(),
                }],
            }
        }

        fn empty() -> Self {
            Self {
                credentials: vec![],
                temp_credentials: vec![],
            }
        }
    }

    impl crate::config::ConfigProvider for MockConfig {
        async fn list_buckets(&self) -> Result<Vec<BucketConfig>, ProxyError> {
            Ok(vec![])
        }
        async fn get_bucket(&self, _: &str) -> Result<Option<BucketConfig>, ProxyError> {
            Ok(None)
        }
        async fn get_role(&self, _: &str) -> Result<Option<RoleConfig>, ProxyError> {
            Ok(None)
        }
        async fn get_credential(
            &self,
            access_key_id: &str,
        ) -> Result<Option<StoredCredential>, ProxyError> {
            Ok(self
                .credentials
                .iter()
                .find(|c| c.access_key_id == access_key_id)
                .cloned())
        }
        async fn store_temporary_credential(
            &self,
            _: &TemporaryCredentials,
        ) -> Result<(), ProxyError> {
            Ok(())
        }
        async fn get_temporary_credential(
            &self,
            access_key_id: &str,
        ) -> Result<Option<TemporaryCredentials>, ProxyError> {
            Ok(self
                .temp_credentials
                .iter()
                .find(|c| c.access_key_id == access_key_id)
                .cloned())
        }
    }

    // ── Test signing helper ───────────────────────────────────────────

    /// Build a valid SigV4 Authorization header value for testing.
    fn sign_request(
        method: &http::Method,
        uri_path: &str,
        query_string: &str,
        headers: &HeaderMap,
        access_key_id: &str,
        secret_access_key: &str,
        date_stamp: &str,
        amz_date: &str,
        region: &str,
        signed_header_names: &[&str],
        payload_hash: &str,
    ) -> String {
        let canonical_headers: String = signed_header_names
            .iter()
            .map(|name| {
                let value = headers
                    .get(*name)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .trim();
                format!("{}:{}\n", name, value)
            })
            .collect();

        let signed_headers_str = signed_header_names.join(";");

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method, uri_path, query_string, canonical_headers, signed_headers_str, payload_hash
        );

        let canonical_request_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
        let credential_scope = format!("{}/{}/s3/aws4_request", date_stamp, region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            amz_date, credential_scope, canonical_request_hash
        );

        let k_date = hmac_sha256(
            format!("AWS4{}", secret_access_key).as_bytes(),
            date_stamp.as_bytes(),
        )
        .unwrap();
        let k_region = hmac_sha256(&k_date, region.as_bytes()).unwrap();
        let k_service = hmac_sha256(&k_region, b"s3").unwrap();
        let signing_key = hmac_sha256(&k_service, b"aws4_request").unwrap();
        let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes()).unwrap());

        format!(
            "AWS4-HMAC-SHA256 Credential={}/{}/{}/s3/aws4_request, SignedHeaders={}, Signature={}",
            access_key_id, date_stamp, region, signed_headers_str, signature
        )
    }

    /// Build headers and auth for a simple GET request.
    fn make_signed_headers(access_key_id: &str, secret_access_key: &str) -> HeaderMap {
        let date_stamp = "20240101";
        let amz_date = "20240101T000000Z";
        let region = "us-east-1";
        let payload_hash = "UNSIGNED-PAYLOAD";

        let mut headers = HeaderMap::new();
        headers.insert("host", "s3.example.com".parse().unwrap());
        headers.insert("x-amz-date", amz_date.parse().unwrap());
        headers.insert("x-amz-content-sha256", payload_hash.parse().unwrap());

        let auth = sign_request(
            &http::Method::GET,
            "/test-bucket/key.txt",
            "",
            &headers,
            access_key_id,
            secret_access_key,
            date_stamp,
            amz_date,
            region,
            &["host", "x-amz-content-sha256", "x-amz-date"],
            payload_hash,
        );
        headers.insert("authorization", auth.parse().unwrap());
        headers
    }

    // ── Tests ─────────────────────────────────────────────────────────

    fn run<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    #[test]
    fn no_auth_header_returns_anonymous() {
        run(async {
            let headers = HeaderMap::new();
            let config = MockConfig::empty();

            let identity = resolve_identity(
                &http::Method::GET,
                "/test-bucket/key.txt",
                "",
                &headers,
                &config,
            )
            .await
            .unwrap();

            assert!(matches!(identity, ResolvedIdentity::Anonymous));
        });
    }

    #[test]
    fn valid_signature_resolves_identity() {
        run(async {
            let secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
            let config = MockConfig::with_credential(secret);
            let headers = make_signed_headers("AKIAIOSFODNN7EXAMPLE", secret);

            let identity = resolve_identity(
                &http::Method::GET,
                "/test-bucket/key.txt",
                "",
                &headers,
                &config,
            )
            .await
            .unwrap();

            assert!(matches!(identity, ResolvedIdentity::LongLived { .. }));
        });
    }

    #[test]
    fn wrong_signature_is_rejected() {
        run(async {
            let real_secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
            let wrong_secret = "WRONGSECRETKEYWRONGSECRETSECRET00000000000";
            let config = MockConfig::with_credential(real_secret);
            // Sign with wrong secret — access_key_id is correct, signature won't match
            let headers = make_signed_headers("AKIAIOSFODNN7EXAMPLE", wrong_secret);

            let err = resolve_identity(
                &http::Method::GET,
                "/test-bucket/key.txt",
                "",
                &headers,
                &config,
            )
            .await
            .unwrap_err();

            assert!(
                matches!(err, ProxyError::SignatureDoesNotMatch),
                "expected SignatureDoesNotMatch, got: {:?}",
                err
            );
        });
    }

    #[test]
    fn garbage_signature_is_rejected() {
        run(async {
            let real_secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
            let config = MockConfig::with_credential(real_secret);

            let mut headers = HeaderMap::new();
            headers.insert("host", "s3.example.com".parse().unwrap());
            headers.insert("x-amz-date", "20240101T000000Z".parse().unwrap());
            headers.insert("x-amz-content-sha256", "UNSIGNED-PAYLOAD".parse().unwrap());
            headers.insert(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20240101/us-east-1/s3/aws4_request, \
                 SignedHeaders=host;x-amz-content-sha256;x-amz-date, \
                 Signature=0000000000000000000000000000000000000000000000000000000000000000"
                    .parse()
                    .unwrap(),
            );

            let err = resolve_identity(
                &http::Method::GET,
                "/test-bucket/key.txt",
                "",
                &headers,
                &config,
            )
            .await
            .unwrap_err();

            assert!(matches!(err, ProxyError::SignatureDoesNotMatch));
        });
    }

    #[test]
    fn unknown_access_key_is_rejected() {
        run(async {
            let config = MockConfig::empty();
            let headers = make_signed_headers("AKIAUNKNOWN000000000", "some-secret");

            let err = resolve_identity(
                &http::Method::GET,
                "/test-bucket/key.txt",
                "",
                &headers,
                &config,
            )
            .await
            .unwrap_err();

            assert!(matches!(err, ProxyError::AccessDenied));
        });
    }

    #[test]
    fn temp_credential_valid_signature_and_token() {
        run(async {
            let secret = "TempSecretKey1234567890EXAMPLE000000000000";
            let session_token = "FwoGZXIvYXdzEBYaDGFiY2RlZjEyMzQ1Ng";
            let config = MockConfig::with_temp_credential(secret, session_token);

            let date_stamp = "20240101";
            let amz_date = "20240101T000000Z";
            let payload_hash = "UNSIGNED-PAYLOAD";

            let mut headers = HeaderMap::new();
            headers.insert("host", "s3.example.com".parse().unwrap());
            headers.insert("x-amz-date", amz_date.parse().unwrap());
            headers.insert("x-amz-content-sha256", payload_hash.parse().unwrap());
            headers.insert("x-amz-security-token", session_token.parse().unwrap());

            let auth = sign_request(
                &http::Method::GET,
                "/test-bucket/key.txt",
                "",
                &headers,
                "ASIATEMP1234EXAMPLE",
                secret,
                date_stamp,
                amz_date,
                "us-east-1",
                &[
                    "host",
                    "x-amz-content-sha256",
                    "x-amz-date",
                    "x-amz-security-token",
                ],
                payload_hash,
            );
            headers.insert("authorization", auth.parse().unwrap());

            let identity = resolve_identity(
                &http::Method::GET,
                "/test-bucket/key.txt",
                "",
                &headers,
                &config,
            )
            .await
            .unwrap();

            assert!(matches!(identity, ResolvedIdentity::Temporary { .. }));
        });
    }

    #[test]
    fn temp_credential_wrong_session_token_is_rejected() {
        run(async {
            let secret = "TempSecretKey1234567890EXAMPLE000000000000";
            let real_token = "FwoGZXIvYXdzEBYaDGFiY2RlZjEyMzQ1Ng";
            let wrong_token = "WRONG_TOKEN_VALUE_HERE";
            let config = MockConfig::with_temp_credential(secret, real_token);

            let date_stamp = "20240101";
            let amz_date = "20240101T000000Z";
            let payload_hash = "UNSIGNED-PAYLOAD";

            let mut headers = HeaderMap::new();
            headers.insert("host", "s3.example.com".parse().unwrap());
            headers.insert("x-amz-date", amz_date.parse().unwrap());
            headers.insert("x-amz-content-sha256", payload_hash.parse().unwrap());
            headers.insert("x-amz-security-token", wrong_token.parse().unwrap());

            let auth = sign_request(
                &http::Method::GET,
                "/test-bucket/key.txt",
                "",
                &headers,
                "ASIATEMP1234EXAMPLE",
                secret,
                date_stamp,
                amz_date,
                "us-east-1",
                &[
                    "host",
                    "x-amz-content-sha256",
                    "x-amz-date",
                    "x-amz-security-token",
                ],
                payload_hash,
            );
            headers.insert("authorization", auth.parse().unwrap());

            let err = resolve_identity(
                &http::Method::GET,
                "/test-bucket/key.txt",
                "",
                &headers,
                &config,
            )
            .await
            .unwrap_err();

            assert!(matches!(err, ProxyError::AccessDenied));
        });
    }

    #[test]
    fn temp_credential_wrong_signature_is_rejected() {
        run(async {
            let real_secret = "TempSecretKey1234567890EXAMPLE000000000000";
            let wrong_secret = "WRONGSECRETKEYWRONGSECRETSECRET00000000000";
            let session_token = "FwoGZXIvYXdzEBYaDGFiY2RlZjEyMzQ1Ng";
            let config = MockConfig::with_temp_credential(real_secret, session_token);

            let date_stamp = "20240101";
            let amz_date = "20240101T000000Z";
            let payload_hash = "UNSIGNED-PAYLOAD";

            let mut headers = HeaderMap::new();
            headers.insert("host", "s3.example.com".parse().unwrap());
            headers.insert("x-amz-date", amz_date.parse().unwrap());
            headers.insert("x-amz-content-sha256", payload_hash.parse().unwrap());
            headers.insert("x-amz-security-token", session_token.parse().unwrap());

            // Sign with wrong secret — session token is correct but sig won't match
            let auth = sign_request(
                &http::Method::GET,
                "/test-bucket/key.txt",
                "",
                &headers,
                "ASIATEMP1234EXAMPLE",
                wrong_secret,
                date_stamp,
                amz_date,
                "us-east-1",
                &[
                    "host",
                    "x-amz-content-sha256",
                    "x-amz-date",
                    "x-amz-security-token",
                ],
                payload_hash,
            );
            headers.insert("authorization", auth.parse().unwrap());

            let err = resolve_identity(
                &http::Method::GET,
                "/test-bucket/key.txt",
                "",
                &headers,
                &config,
            )
            .await
            .unwrap_err();

            assert!(
                matches!(err, ProxyError::SignatureDoesNotMatch),
                "expected SignatureDoesNotMatch, got: {:?}",
                err
            );
        });
    }

    #[test]
    fn disabled_credential_is_rejected_before_sig_check() {
        run(async {
            let secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
            let mut config = MockConfig::with_credential(secret);
            config.credentials[0].enabled = false;

            let headers = make_signed_headers("AKIAIOSFODNN7EXAMPLE", secret);

            let err = resolve_identity(
                &http::Method::GET,
                "/test-bucket/key.txt",
                "",
                &headers,
                &config,
            )
            .await
            .unwrap_err();

            assert!(matches!(err, ProxyError::AccessDenied));
        });
    }

    // ── Prefix boundary tests ─────────────────────────────────────

    #[test]
    fn prefix_with_slash_matches_children() {
        assert!(key_matches_prefix("data/file.txt", "data/"));
        assert!(key_matches_prefix("data/sub/file.txt", "data/"));
    }

    #[test]
    fn prefix_without_slash_enforces_boundary() {
        // Should match exact or with / boundary
        assert!(key_matches_prefix("data/file.txt", "data"));
        assert!(key_matches_prefix("data", "data"));
        // Should NOT match sibling paths
        assert!(!key_matches_prefix("data-private/secret.txt", "data"));
        assert!(!key_matches_prefix("database/dump.sql", "data"));
    }

    #[test]
    fn empty_prefix_matches_everything() {
        assert!(key_matches_prefix("anything/at/all.txt", ""));
        assert!(key_matches_prefix("", ""));
    }

    #[test]
    fn prefix_no_match() {
        assert!(!key_matches_prefix("other/file.txt", "data/"));
        assert!(!key_matches_prefix("other/file.txt", "data"));
    }
}

/// Check if a key falls under an authorized prefix.
///
/// If the prefix already ends with `/`, a plain `starts_with` is sufficient.
/// Otherwise we require that the key either equals the prefix exactly or
/// that the character immediately after the prefix is `/`. This prevents
/// a prefix like `data` from matching `data-private/secret.txt`.
fn key_matches_prefix(key: &str, prefix: &str) -> bool {
    if prefix.ends_with('/') || prefix.is_empty() {
        return key.starts_with(prefix);
    }
    // Prefix does not end with '/' — require an exact match or a '/' boundary
    key == prefix || key.starts_with(&format!("{}/", prefix))
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
        scope
            .prefixes
            .iter()
            .any(|prefix| key_matches_prefix(&key, prefix))
    });

    if authorized {
        Ok(())
    } else {
        Err(ProxyError::AccessDenied)
    }
}
