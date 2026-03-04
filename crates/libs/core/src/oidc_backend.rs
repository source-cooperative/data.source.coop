//! Trait for OIDC-based backend credential resolution.
//!
//! When a bucket is configured with `auth_type=oidc`, the proxy mints a
//! self-signed JWT and exchanges it with the cloud provider's STS for
//! temporary credentials. The resolved credentials are injected into the
//! `BucketConfig.backend_options` so the existing builder pipeline works
//! unmodified.
//!
//! [`NoOidcAuth`] is the default no-op implementation used when no OIDC
//! provider is configured.

use crate::error::ProxyError;
use crate::maybe_send::MaybeSend;
use crate::types::BucketConfig;
use std::future::Future;

/// Resolves backend credentials via OIDC token exchange.
///
/// Called at the top of `dispatch_operation()` before the config reaches
/// `create_store()` / `create_signer()`. Implementations may return the
/// config unchanged (no `auth_type=oidc`) or inject temporary credentials.
pub trait OidcBackendAuth: MaybeSend + 'static {
    fn resolve_credentials(
        &self,
        config: &BucketConfig,
    ) -> impl Future<Output = Result<BucketConfig, ProxyError>> + MaybeSend;
}

/// No-op implementation — returns config unchanged.
///
/// If a bucket specifies `auth_type=oidc` but no OIDC provider is
/// configured, this returns a `ConfigError`.
pub struct NoOidcAuth;

impl OidcBackendAuth for NoOidcAuth {
    async fn resolve_credentials(&self, config: &BucketConfig) -> Result<BucketConfig, ProxyError> {
        if config.option("auth_type") == Some("oidc") {
            return Err(ProxyError::ConfigError(
                "bucket requires auth_type=oidc but no OIDC provider is configured".into(),
            ));
        }
        Ok(config.clone())
    }
}
