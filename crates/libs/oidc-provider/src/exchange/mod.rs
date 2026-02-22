//! Credential exchange — trade a self-signed JWT for cloud provider credentials.

pub mod aws;
#[cfg(feature = "azure")]
pub mod azure;
#[cfg(feature = "gcp")]
pub mod gcp;

use crate::{CloudCredentials, HttpExchange, OidcProviderError};

/// Trait for exchanging a self-signed JWT for cloud provider credentials.
///
/// Each cloud provider has a different token exchange flow:
/// - AWS: `AssumeRoleWithWebIdentity` via STS
/// - Azure: Federated token exchange via Azure AD
/// - GCP: STS token exchange + `generateAccessToken` via IAM
pub trait CredentialExchange<H: HttpExchange>: s3_proxy_core::maybe_send::MaybeSend + s3_proxy_core::maybe_send::MaybeSync {
    fn exchange(
        &self,
        http: &H,
        jwt: &str,
    ) -> impl std::future::Future<Output = Result<CloudCredentials, OidcProviderError>> + s3_proxy_core::maybe_send::MaybeSend;
}
