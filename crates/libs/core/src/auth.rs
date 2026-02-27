//! Authentication and authorization.
//!
//! Handles:
//! - SigV4 request verification (incoming requests from clients)
//! - Identity resolution (mapping access key → principal)
//! - Authorization (checking if an identity can perform an operation)

use crate::config::ConfigProvider;
use crate::error::ProxyError;
use crate::sealed_token::TokenKey;
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

    // SigV4 requires query parameters sorted alphabetically by key (then value).
    // The raw query string from the URL may not be sorted, but the client SDK
    // sorts them when constructing the canonical request for signing.
    let canonical_query = canonicalize_query_string(query_string);

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method, uri_path, canonical_query, canonical_headers, signed_headers_str, payload_hash
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

    let matched = constant_time_eq(expected_signature.as_bytes(), auth.signature.as_bytes());

    if !matched {
        tracing::warn!(
            canonical_request = %canonical_request,
            string_to_sign = %string_to_sign,
            expected_signature = %expected_signature,
            provided_signature = %auth.signature,
            "SigV4 signature mismatch — compare canonical_request with client-side (aws --debug)"
        );
    }

    Ok(matched)
}

/// Sort query string parameters for SigV4 canonical request construction.
fn canonicalize_query_string(query: &str) -> String {
    if query.is_empty() {
        return String::new();
    }
    let mut parts: Vec<&str> = query.split('&').collect();
    parts.sort_unstable();
    parts.join("&")
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
    token_key: Option<&TokenKey>,
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

    // Temporary credentials: decrypt the session token to recover credentials
    if let Some(session_token) = headers
        .get("x-amz-security-token")
        .and_then(|v| v.to_str().ok())
    {
        let key = token_key.ok_or_else(|| {
            tracing::warn!("session token present but no token_key configured");
            ProxyError::AccessDenied
        })?;

        match key.unseal(session_token)? {
            Some(creds) => {
                if !constant_time_eq(
                    sig.access_key_id.as_bytes(),
                    creds.access_key_id.as_bytes(),
                ) {
                    tracing::warn!(
                        header_key = %sig.access_key_id,
                        sealed_key = %creds.access_key_id,
                        "access key mismatch between auth header and sealed token"
                    );
                    return Err(ProxyError::AccessDenied);
                }
                if !verify_sigv4_signature(
                    method,
                    uri_path,
                    query_string,
                    headers,
                    &sig,
                    &creds.secret_access_key,
                    payload_hash,
                )? {
                    return Err(ProxyError::SignatureDoesNotMatch);
                }
                tracing::debug!(
                    access_key = %creds.access_key_id,
                    role = %creds.assumed_role_id,
                    scopes = ?creds.allowed_scopes,
                    "sealed token identity resolved"
                );
                return Ok(ResolvedIdentity::Temporary {
                    credentials: creds,
                });
            }
            None => {
                tracing::warn!("session token could not be unsealed (decryption failed)");
                return Err(ProxyError::AccessDenied);
            }
        }
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
            }
        }

        fn empty() -> Self {
            Self {
                credentials: vec![],
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

        // AWS SDKs sort query parameters when constructing the canonical request
        let canonical_query = canonicalize_query_string(query_string);

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method, uri_path, canonical_query, canonical_headers, signed_headers_str, payload_hash
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
                None,
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
                None,
            )
            .await
            .unwrap();

            assert!(matches!(identity, ResolvedIdentity::LongLived { .. }));
        });
    }

    #[test]
    fn valid_signature_with_unsorted_query_params() {
        run(async {
            let secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
            let config = MockConfig::with_credential(secret);

            let date_stamp = "20240101";
            let amz_date = "20240101T000000Z";
            let payload_hash = "UNSIGNED-PAYLOAD";

            let mut headers = HeaderMap::new();
            headers.insert("host", "s3.example.com".parse().unwrap());
            headers.insert("x-amz-date", amz_date.parse().unwrap());
            headers.insert("x-amz-content-sha256", payload_hash.parse().unwrap());

            // Sign with sorted query (as AWS SDKs do internally)
            let auth = sign_request(
                &http::Method::GET,
                "/test-bucket",
                "list-type=2&prefix=&delimiter=%2F&encoding-type=url",
                &headers,
                "AKIAIOSFODNN7EXAMPLE",
                secret,
                date_stamp,
                amz_date,
                "us-east-1",
                &["host", "x-amz-content-sha256", "x-amz-date"],
                payload_hash,
            );
            headers.insert("authorization", auth.parse().unwrap());

            // Pass UNSORTED query string (as it arrives from the raw URL)
            let identity = resolve_identity(
                &http::Method::GET,
                "/test-bucket",
                "list-type=2&prefix=&delimiter=%2F&encoding-type=url",
                &headers,
                &config,
                None,
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
                None,
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
                None,
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
                None,
            )
            .await
            .unwrap_err();

            assert!(matches!(err, ProxyError::AccessDenied));
        });
    }

    #[test]
    fn sealed_token_wrong_session_token_is_rejected() {
        use crate::sealed_token::TokenKey;

        run(async {
            let key_bytes = [0x42u8; 32];
            let encoded = base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                key_bytes,
            );
            let token_key = TokenKey::from_base64(&encoded).unwrap();
            let config = MockConfig::empty();

            let secret = "TempSecretKey1234567890EXAMPLE000000000000";
            let wrong_token = "NOT_A_SEALED_TOKEN_AT_ALL";

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
                Some(&token_key),
            )
            .await
            .unwrap_err();

            assert!(matches!(err, ProxyError::AccessDenied));
        });
    }

    #[test]
    fn sealed_token_wrong_signature_is_rejected() {
        use crate::sealed_token::TokenKey;
        use crate::types::AccessScope;

        run(async {
            let key_bytes = [0x42u8; 32];
            let encoded = base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                key_bytes,
            );
            let token_key = TokenKey::from_base64(&encoded).unwrap();

            let real_secret = "TempSecretKey1234567890EXAMPLE000000000000";
            let wrong_secret = "WRONGSECRETKEYWRONGSECRETSECRET00000000000";
            let creds = TemporaryCredentials {
                access_key_id: "ASIATEMP1234EXAMPLE".into(),
                secret_access_key: real_secret.into(),
                session_token: String::new(),
                expiration: chrono::Utc::now() + chrono::Duration::hours(1),
                allowed_scopes: vec![AccessScope {
                    bucket: "test-bucket".into(),
                    prefixes: vec![],
                    actions: vec![Action::GetObject],
                }],
                assumed_role_id: "role-1".into(),
                source_identity: "test".into(),
            };

            let sealed = token_key.seal(&creds).unwrap();
            let config = MockConfig::empty();

            let date_stamp = "20240101";
            let amz_date = "20240101T000000Z";
            let payload_hash = "UNSIGNED-PAYLOAD";

            let mut headers = HeaderMap::new();
            headers.insert("host", "s3.example.com".parse().unwrap());
            headers.insert("x-amz-date", amz_date.parse().unwrap());
            headers.insert("x-amz-content-sha256", payload_hash.parse().unwrap());
            headers.insert("x-amz-security-token", sealed.parse().unwrap());

            // Sign with wrong secret — sealed token is valid but sig won't match
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
                Some(&token_key),
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
                None,
            )
            .await
            .unwrap_err();

            assert!(matches!(err, ProxyError::AccessDenied));
        });
    }

    // ── SigV4 spec compliance tests ──────────────────────────────────

    /// Validate our SigV4 implementation against the official AWS test suite.
    /// Test vector: "get-vanilla" from
    /// https://docs.aws.amazon.com/general/latest/gr/signature-v4-test-suite.html
    #[test]
    fn sigv4_test_vector_get_vanilla() {
        let access_key_id = "AKIDEXAMPLE";
        let secret = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
        let date_stamp = "20150830";
        let amz_date = "20150830T123600Z";
        let region = "us-east-1";
        let service = "service";
        let payload_hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

        let mut headers = HeaderMap::new();
        headers.insert("host", "example.amazonaws.com".parse().unwrap());
        headers.insert("x-amz-date", amz_date.parse().unwrap());

        // Build the canonical request exactly as the spec defines:
        // GET\n/\n\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\n\nhost;x-amz-date\ne3b0c44...
        let auth = SigV4Auth {
            access_key_id: access_key_id.to_string(),
            date_stamp: date_stamp.to_string(),
            region: region.to_string(),
            service: service.to_string(),
            signed_headers: vec!["host".to_string(), "x-amz-date".to_string()],
            signature: "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
                .to_string(),
        };

        let result = verify_sigv4_signature(
            &http::Method::GET,
            "/",
            "",
            &headers,
            &auth,
            secret,
            payload_hash,
        )
        .unwrap();

        assert!(result, "AWS SigV4 test vector 'get-vanilla' must pass");
    }

    /// Test vector: "get-vanilla-query-order-key" — verifies query parameter sorting.
    /// Parameters Param2 and Param1 must be sorted alphabetically.
    #[test]
    fn sigv4_test_vector_query_order() {
        let secret = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
        let date_stamp = "20150830";
        let amz_date = "20150830T123600Z";
        let payload_hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

        let mut headers = HeaderMap::new();
        headers.insert("host", "example.amazonaws.com".parse().unwrap());
        headers.insert("x-amz-date", amz_date.parse().unwrap());

        let auth = SigV4Auth {
            access_key_id: "AKIDEXAMPLE".to_string(),
            date_stamp: date_stamp.to_string(),
            region: "us-east-1".to_string(),
            service: "service".to_string(),
            signed_headers: vec!["host".to_string(), "x-amz-date".to_string()],
            signature: "b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500"
                .to_string(),
        };

        // Pass UNSORTED query — our canonicalization should sort to Param1=value1&Param2=value2
        let result = verify_sigv4_signature(
            &http::Method::GET,
            "/",
            "Param2=value2&Param1=value1",
            &headers,
            &auth,
            secret,
            payload_hash,
        )
        .unwrap();

        assert!(
            result,
            "AWS SigV4 test vector 'get-vanilla-query-order-key' must pass"
        );
    }

    /// Realistic S3 ListObjectsV2 request with host:port, security token,
    /// and unsorted query parameters — mirrors what `aws s3 ls` sends.
    #[test]
    fn sigv4_list_objects_with_security_token_and_port() {
        let secret = "TempSecretKey1234567890EXAMPLE000000000000";
        let session_token = "FwoGZXIvYXdzEBYaDGFiY2RlZjEyMzQ1Ng";
        let date_stamp = "20240101";
        let amz_date = "20240101T000000Z";
        let payload_hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

        let mut headers = HeaderMap::new();
        headers.insert("host", "localhost:8787".parse().unwrap());
        headers.insert("x-amz-date", amz_date.parse().unwrap());
        headers.insert("x-amz-content-sha256", payload_hash.parse().unwrap());
        headers.insert("x-amz-security-token", session_token.parse().unwrap());

        // Sign with sorted query (as AWS SDKs do)
        let auth = sign_request(
            &http::Method::GET,
            "/private-uploads",
            "list-type=2&prefix=&delimiter=%2F&encoding-type=url",
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

        // Verify with UNSORTED query (as it arrives from the raw URL)
        let sig = parse_sigv4_auth(
            headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .unwrap();

        let result = verify_sigv4_signature(
            &http::Method::GET,
            "/private-uploads",
            "list-type=2&prefix=&delimiter=%2F&encoding-type=url",
            &headers,
            &sig,
            secret,
            payload_hash,
        )
        .unwrap();

        assert!(result, "S3 ListObjects with security token and host:port must verify");
    }

    // ── Sealed token tests ──────────────────────────────────────────

    #[test]
    fn sealed_token_round_trip() {
        use crate::sealed_token::TokenKey;
        use crate::types::AccessScope;

        run(async {
            let key_bytes = [0x42u8; 32];
            let encoded = base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                key_bytes,
            );
            let token_key = TokenKey::from_base64(&encoded).unwrap();

            let secret = "TempSecretKey1234567890EXAMPLE000000000000";
            let creds = TemporaryCredentials {
                access_key_id: "ASIATEMP1234EXAMPLE".into(),
                secret_access_key: secret.into(),
                session_token: String::new(), // will be replaced by seal
                expiration: chrono::Utc::now() + chrono::Duration::hours(1),
                allowed_scopes: vec![AccessScope {
                    bucket: "test-bucket".into(),
                    prefixes: vec![],
                    actions: vec![Action::GetObject],
                }],
                assumed_role_id: "role-1".into(),
                source_identity: "test".into(),
            };

            let sealed = token_key.seal(&creds).unwrap();
            let config = MockConfig::empty();

            let date_stamp = "20240101";
            let amz_date = "20240101T000000Z";
            let payload_hash = "UNSIGNED-PAYLOAD";

            let mut headers = HeaderMap::new();
            headers.insert("host", "s3.example.com".parse().unwrap());
            headers.insert("x-amz-date", amz_date.parse().unwrap());
            headers.insert("x-amz-content-sha256", payload_hash.parse().unwrap());
            headers.insert("x-amz-security-token", sealed.parse().unwrap());

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
                Some(&token_key),
            )
            .await
            .unwrap();

            assert!(matches!(identity, ResolvedIdentity::Temporary { .. }));
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
        tracing::warn!(
            action = ?action,
            bucket = %bucket,
            key = %key,
            scopes = ?scopes,
            "authorization denied — no scope grants access"
        );
        Err(ProxyError::AccessDenied)
    }
}
