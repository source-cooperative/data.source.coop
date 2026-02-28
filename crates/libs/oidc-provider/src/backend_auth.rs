//! OIDC-based backend credential resolution.
//!
//! When a bucket's `backend_options` contains `auth_type=oidc`, the proxy
//! mints a self-signed JWT and exchanges it for temporary cloud credentials
//! via the cloud provider's STS. The resolved credentials are injected back
//! into the config so the existing builder pipeline works unmodified.

use source_coop_core::error::ProxyError;
use source_coop_core::oidc_backend::OidcBackendAuth;
use source_coop_core::types::BucketConfig;

use crate::exchange::aws::AwsExchange;
use crate::{HttpExchange, OidcCredentialProvider};

/// AWS OIDC backend auth — exchanges a self-signed JWT for temporary
/// AWS credentials via `AssumeRoleWithWebIdentity`.
pub struct AwsOidcBackendAuth<H: HttpExchange> {
    provider: OidcCredentialProvider<H>,
}

impl<H: HttpExchange> AwsOidcBackendAuth<H> {
    pub fn new(provider: OidcCredentialProvider<H>) -> Self {
        Self { provider }
    }

    async fn resolve_aws(&self, config: &BucketConfig) -> Result<BucketConfig, ProxyError> {
        let role_arn = config.option("oidc_role_arn").ok_or_else(|| {
            ProxyError::ConfigError(
                "auth_type=oidc requires 'oidc_role_arn' in backend_options".into(),
            )
        })?;
        let subject = config.option("oidc_subject").unwrap_or("s3-proxy");

        let exchange = AwsExchange::new(role_arn.to_string());
        let creds = self
            .provider
            .get_credentials(role_arn, &exchange, subject, &[])
            .await?;

        let mut resolved = config.clone();
        resolved
            .backend_options
            .insert("access_key_id".into(), creds.access_key_id.clone());
        resolved
            .backend_options
            .insert("secret_access_key".into(), creds.secret_access_key.clone());
        resolved
            .backend_options
            .insert("token".into(), creds.session_token.clone());

        // Remove OIDC-specific keys so they don't confuse the builder.
        resolved.backend_options.remove("auth_type");
        resolved.backend_options.remove("oidc_role_arn");
        resolved.backend_options.remove("oidc_subject");

        Ok(resolved)
    }
}

impl<H: HttpExchange> OidcBackendAuth for AwsOidcBackendAuth<H> {
    async fn resolve_credentials(&self, config: &BucketConfig) -> Result<BucketConfig, ProxyError> {
        if config.option("auth_type") != Some("oidc") {
            return Ok(config.clone());
        }

        // TODO: dispatch on backend_type for Azure/GCP when those exchanges are wired up.
        match config.backend_type.as_str() {
            "s3" => self.resolve_aws(config).await,
            other => Err(ProxyError::ConfigError(format!(
                "OIDC backend auth not yet supported for backend_type '{other}'"
            ))),
        }
    }
}

/// Wrapper enum that runtimes use as a single concrete `O` type.
///
/// `Enabled` holds the live OIDC provider; `Disabled` is the no-op fallback.
/// When disabled and a bucket specifies `auth_type=oidc`, a `ConfigError`
/// is returned (same as `NoOidcAuth`).
pub enum MaybeOidcAuth<H: HttpExchange> {
    Enabled(AwsOidcBackendAuth<H>),
    Disabled,
}

impl<H: HttpExchange> OidcBackendAuth for MaybeOidcAuth<H> {
    async fn resolve_credentials(&self, config: &BucketConfig) -> Result<BucketConfig, ProxyError> {
        match self {
            MaybeOidcAuth::Enabled(auth) => auth.resolve_credentials(config).await,
            MaybeOidcAuth::Disabled => {
                if config.option("auth_type") == Some("oidc") {
                    Err(ProxyError::ConfigError(
                        "bucket requires auth_type=oidc but no OIDC provider is configured".into(),
                    ))
                } else {
                    Ok(config.clone())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jwt::JwtSigner;
    use crate::OidcProviderError;
    use chrono::{Duration, Utc};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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
                            <AccessKeyId>AKID_OIDC</AccessKeyId>
                            <SecretAccessKey>secret_oidc</SecretAccessKey>
                            <SessionToken>token_oidc</SessionToken>
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

    fn oidc_bucket_config() -> BucketConfig {
        let mut opts = HashMap::new();
        opts.insert("auth_type".into(), "oidc".into());
        opts.insert("oidc_role_arn".into(), "arn:aws:iam::123:role/Test".into());
        opts.insert(
            "endpoint".into(),
            "https://s3.us-east-1.amazonaws.com".into(),
        );
        opts.insert("bucket_name".into(), "my-bucket".into());
        opts.insert("region".into(), "us-east-1".into());
        BucketConfig {
            name: "test".into(),
            backend_type: "s3".into(),
            backend_prefix: None,
            anonymous_access: false,
            allowed_roles: vec![],
            backend_options: opts,
        }
    }

    fn static_bucket_config() -> BucketConfig {
        let mut opts = HashMap::new();
        opts.insert("access_key_id".into(), "AKID_STATIC".into());
        opts.insert("secret_access_key".into(), "secret_static".into());
        opts.insert(
            "endpoint".into(),
            "https://s3.us-east-1.amazonaws.com".into(),
        );
        opts.insert("bucket_name".into(), "my-bucket".into());
        BucketConfig {
            name: "test".into(),
            backend_type: "s3".into(),
            backend_prefix: None,
            anonymous_access: false,
            allowed_roles: vec![],
            backend_options: opts,
        }
    }

    #[tokio::test]
    async fn resolve_injects_creds_for_oidc_bucket() {
        let http = MockHttp::new();
        let provider = OidcCredentialProvider::new(
            test_signer(),
            http,
            "https://issuer.example.com".into(),
            "sts.amazonaws.com".into(),
        );
        let auth = AwsOidcBackendAuth::new(provider);

        let config = oidc_bucket_config();
        let resolved = auth.resolve_credentials(&config).await.unwrap();

        assert_eq!(resolved.option("access_key_id"), Some("AKID_OIDC"));
        assert_eq!(resolved.option("secret_access_key"), Some("secret_oidc"));
        assert_eq!(resolved.option("token"), Some("token_oidc"));
        assert!(resolved.option("auth_type").is_none());
        assert!(resolved.option("oidc_role_arn").is_none());
    }

    #[tokio::test]
    async fn resolve_passes_through_static_bucket() {
        let http = MockHttp::new();
        let provider = OidcCredentialProvider::new(
            test_signer(),
            http.clone(),
            "https://issuer.example.com".into(),
            "sts.amazonaws.com".into(),
        );
        let auth = AwsOidcBackendAuth::new(provider);

        let config = static_bucket_config();
        let resolved = auth.resolve_credentials(&config).await.unwrap();

        assert_eq!(resolved.option("access_key_id"), Some("AKID_STATIC"));
        assert_eq!(http.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn maybe_disabled_errors_on_oidc_bucket() {
        let auth: MaybeOidcAuth<MockHttp> = MaybeOidcAuth::Disabled;
        let config = oidc_bucket_config();
        let err = auth.resolve_credentials(&config).await.unwrap_err();
        assert!(err.to_string().contains("no OIDC provider is configured"));
    }

    #[tokio::test]
    async fn maybe_disabled_passes_through_static_bucket() {
        let auth: MaybeOidcAuth<MockHttp> = MaybeOidcAuth::Disabled;
        let config = static_bucket_config();
        let resolved = auth.resolve_credentials(&config).await.unwrap();
        assert_eq!(resolved.option("access_key_id"), Some("AKID_STATIC"));
    }
}
