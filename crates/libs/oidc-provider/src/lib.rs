//! OIDC provider for outbound authentication.
//!
//! This crate enables the proxy to act as its own OIDC identity provider:
//!
//! 1. **JWT signing** — mint JWTs signed with the proxy's RSA private key
//! 2. **JWKS serving** — expose the corresponding public key as a JWK set
//! 3. **OIDC discovery** — generate `.well-known/openid-configuration` responses
//! 4. **Credential exchange** — trade self-signed JWTs for cloud provider
//!    credentials (AWS STS, Azure AD, GCP STS)
//!
//! The crate is runtime-agnostic: HTTP calls are abstracted behind an
//! [`HttpExchange`] trait so that each runtime (reqwest, Fetch API, etc.)
//! can provide its own implementation.

pub mod backend_auth;
pub mod cache;
pub mod discovery;
pub mod exchange;
pub mod jwks;
pub mod jwt;

use std::sync::Arc;

use cache::CredentialCache;
use exchange::CredentialExchange;
use jwt::JwtSigner;

/// Temporary cloud credentials obtained via token exchange.
#[derive(Debug, Clone)]
pub struct CloudCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// HTTP client abstraction for outbound requests (STS token exchange).
///
/// Each runtime provides its own implementation — `reqwest` on native,
/// `Fetch` on Cloudflare Workers.
pub trait HttpExchange:
    Clone + source_coop_core::maybe_send::MaybeSend + source_coop_core::maybe_send::MaybeSync + 'static
{
    fn post_form(
        &self,
        url: &str,
        form: &[(&str, &str)],
    ) -> impl std::future::Future<Output = Result<String, OidcProviderError>>
           + source_coop_core::maybe_send::MaybeSend;
}

/// Top-level provider that combines signing, exchange, and caching.
pub struct OidcCredentialProvider<H: HttpExchange> {
    signer: JwtSigner,
    cache: CredentialCache,
    http: H,
    issuer: String,
    audience: String,
}

impl<H: HttpExchange> OidcCredentialProvider<H> {
    pub fn new(signer: JwtSigner, http: H, issuer: String, audience: String) -> Self {
        Self {
            signer,
            cache: CredentialCache::new(),
            http,
            issuer,
            audience,
        }
    }

    /// Get credentials for a backend, using cached values when available.
    ///
    /// `exchange` describes how to trade the self-signed JWT for cloud
    /// credentials (AWS, Azure, GCP). `cache_key` identifies the backend
    /// for caching purposes (e.g. the role ARN).
    pub async fn get_credentials<E: CredentialExchange<H>>(
        &self,
        cache_key: &str,
        exchange: &E,
        subject: &str,
        extra_claims: &[(&str, &str)],
    ) -> Result<Arc<CloudCredentials>, OidcProviderError> {
        // Check cache first
        if let Some(creds) = self.cache.get(cache_key) {
            return Ok(creds);
        }

        // Mint a JWT
        let token = self
            .signer
            .sign(subject, &self.issuer, &self.audience, extra_claims)?;

        // Exchange it for cloud credentials
        let creds: CloudCredentials = exchange.exchange(&self.http, &token).await?;
        let creds = Arc::new(creds);

        // Cache
        self.cache.put(cache_key.to_string(), creds.clone());

        Ok(creds)
    }

    /// Access the underlying signer (e.g. for JWKS generation).
    pub fn signer(&self) -> &JwtSigner {
        &self.signer
    }
}

/// Errors produced by this crate.
#[derive(Debug, thiserror::Error)]
pub enum OidcProviderError {
    #[error("RSA key error: {0}")]
    KeyError(String),

    #[error("JWT signing error: {0}")]
    SigningError(String),

    #[error("credential exchange failed: {0}")]
    ExchangeError(String),

    #[error("HTTP error: {0}")]
    HttpError(String),
}

impl From<OidcProviderError> for source_coop_core::error::ProxyError {
    fn from(e: OidcProviderError) -> Self {
        source_coop_core::error::ProxyError::Internal(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock HTTP client that records calls and returns a preset AWS STS response.
    #[derive(Clone)]
    struct MockHttp {
        call_count: Arc<AtomicUsize>,
    }

    impl MockHttp {
        fn new() -> Self {
            Self {
                call_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn calls(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    impl HttpExchange for MockHttp {
        async fn post_form(
            &self,
            _url: &str,
            _form: &[(&str, &str)],
        ) -> Result<String, OidcProviderError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let exp = (Utc::now() + Duration::hours(1)).to_rfc3339();
            Ok(format!(
                r#"<AssumeRoleWithWebIdentityResponse>
                    <AssumeRoleWithWebIdentityResult>
                        <Credentials>
                            <AccessKeyId>AKID_MOCK</AccessKeyId>
                            <SecretAccessKey>secret_mock</SecretAccessKey>
                            <SessionToken>token_mock</SessionToken>
                            <Expiration>{exp}</Expiration>
                        </Credentials>
                    </AssumeRoleWithWebIdentityResult>
                </AssumeRoleWithWebIdentityResponse>"#
            ))
        }
    }

    fn test_signer() -> JwtSigner {
        use rsa::pkcs8::EncodePrivateKey;
        let mut rng = rand::rngs::OsRng;
        let key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let pem = key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
        JwtSigner::from_pem(&pem, "test-kid".into(), 300).unwrap()
    }

    #[tokio::test]
    async fn get_credentials_returns_fresh_on_first_call() {
        let http = MockHttp::new();
        let provider = OidcCredentialProvider::new(
            test_signer(),
            http.clone(),
            "https://issuer.example.com".into(),
            "sts.amazonaws.com".into(),
        );

        let exchange = exchange::aws::AwsExchange::new("arn:aws:iam::123:role/Test".into());
        let creds = provider
            .get_credentials("role-a", &exchange, "my-sub", &[])
            .await
            .unwrap();

        assert_eq!(creds.access_key_id, "AKID_MOCK");
        assert_eq!(http.calls(), 1);
    }

    #[tokio::test]
    async fn get_credentials_uses_cache_on_second_call() {
        let http = MockHttp::new();
        let provider = OidcCredentialProvider::new(
            test_signer(),
            http.clone(),
            "https://issuer.example.com".into(),
            "sts.amazonaws.com".into(),
        );

        let exchange = exchange::aws::AwsExchange::new("arn:aws:iam::123:role/Test".into());

        // First call — hits mock HTTP
        let creds1 = provider
            .get_credentials("role-a", &exchange, "sub", &[])
            .await
            .unwrap();
        assert_eq!(http.calls(), 1);

        // Second call — should use cache, no additional HTTP call
        let creds2 = provider
            .get_credentials("role-a", &exchange, "sub", &[])
            .await
            .unwrap();
        assert_eq!(http.calls(), 1);
        assert_eq!(creds1.access_key_id, creds2.access_key_id);
    }

    #[tokio::test]
    async fn different_cache_keys_make_separate_calls() {
        let http = MockHttp::new();
        let provider = OidcCredentialProvider::new(
            test_signer(),
            http.clone(),
            "https://issuer.example.com".into(),
            "sts.amazonaws.com".into(),
        );

        let exchange = exchange::aws::AwsExchange::new("arn:aws:iam::123:role/Test".into());

        provider
            .get_credentials("role-a", &exchange, "sub", &[])
            .await
            .unwrap();
        provider
            .get_credentials("role-b", &exchange, "sub", &[])
            .await
            .unwrap();

        assert_eq!(http.calls(), 2);
    }

    #[test]
    fn signed_jwt_is_verifiable_via_jwks_public_key() {
        use base64::Engine;
        use rsa::pkcs1v15::VerifyingKey;
        use rsa::signature::Verifier;
        use rsa::{BigUint, RsaPublicKey};

        let signer = test_signer();

        // Sign a JWT
        let token = signer.sign("sub", "iss", "aud", &[]).unwrap();

        // Generate JWKS from the same signer
        let jwks_str = jwks::jwks_json(signer.public_key(), signer.kid());
        let jwks: serde_json::Value = serde_json::from_str(&jwks_str).unwrap();

        // Extract public key from JWKS
        let key = &jwks["keys"][0];
        let b64 = &base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let n = BigUint::from_bytes_be(&b64.decode(key["n"].as_str().unwrap()).unwrap());
        let e = BigUint::from_bytes_be(&b64.decode(key["e"].as_str().unwrap()).unwrap());
        let reconstructed_key = RsaPublicKey::new(n, e).unwrap();

        // Verify signature using the JWKS-derived key
        let parts: Vec<&str> = token.split('.').collect();
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let sig_bytes = b64.decode(parts[2]).unwrap();
        let signature = rsa::pkcs1v15::Signature::try_from(sig_bytes.as_slice()).unwrap();

        let verifying_key = VerifyingKey::<sha2::Sha256>::new(reconstructed_key);
        verifying_key
            .verify(signing_input.as_bytes(), &signature)
            .expect("JWT signed by JwtSigner should be verifiable via JWKS public key");
    }

    #[test]
    fn error_converts_to_proxy_error() {
        let err = OidcProviderError::ExchangeError("test".into());
        let proxy_err: source_coop_core::error::ProxyError = err.into();
        assert!(proxy_err.to_string().contains("test"));
        assert_eq!(proxy_err.status_code(), 500);
    }
}
