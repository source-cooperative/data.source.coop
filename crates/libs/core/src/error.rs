//! Error types for the proxy.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("bucket not found: {0}")]
    BucketNotFound(String),

    #[error("access denied")]
    AccessDenied,

    #[error("signature mismatch")]
    SignatureDoesNotMatch,

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("missing authentication")]
    MissingAuth,

    #[error("expired credentials")]
    ExpiredCredentials,

    #[error("invalid OIDC token: {0}")]
    InvalidOidcToken(String),

    #[error("role not found: {0}")]
    RoleNotFound(String),

    #[error("backend error: {0}")]
    BackendError(String),

    #[error("config error: {0}")]
    ConfigError(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl ProxyError {
    /// Return the S3-compatible XML error code.
    pub fn s3_error_code(&self) -> &'static str {
        match self {
            Self::BucketNotFound(_) => "NoSuchBucket",
            Self::AccessDenied => "AccessDenied",
            Self::SignatureDoesNotMatch => "SignatureDoesNotMatch",
            Self::InvalidRequest(_) => "InvalidRequest",
            Self::MissingAuth => "AccessDenied",
            Self::ExpiredCredentials => "ExpiredToken",
            Self::InvalidOidcToken(_) => "InvalidIdentityToken",
            Self::RoleNotFound(_) => "AccessDenied",
            Self::BackendError(_) => "InternalError",
            Self::ConfigError(_) => "InternalError",
            Self::Internal(_) => "InternalError",
        }
    }

    /// HTTP status code for this error.
    pub fn status_code(&self) -> u16 {
        match self {
            Self::BucketNotFound(_) => 404,
            Self::AccessDenied | Self::MissingAuth | Self::ExpiredCredentials => 403,
            Self::SignatureDoesNotMatch => 403,
            Self::InvalidRequest(_) => 400,
            Self::InvalidOidcToken(_) => 400,
            Self::RoleNotFound(_) => 403,
            Self::BackendError(_) | Self::ConfigError(_) | Self::Internal(_) => 500,
        }
    }
}
