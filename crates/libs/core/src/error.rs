//! Error types for the proxy.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("bucket not found: {0}")]
    BucketNotFound(String),

    #[error("no such key: {0}")]
    NoSuchKey(String),

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

    #[error("precondition failed")]
    PreconditionFailed,

    #[error("not modified")]
    NotModified,

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
            Self::NoSuchKey(_) => "NoSuchKey",
            Self::AccessDenied => "AccessDenied",
            Self::SignatureDoesNotMatch => "SignatureDoesNotMatch",
            Self::InvalidRequest(_) => "InvalidRequest",
            Self::MissingAuth => "AccessDenied",
            Self::ExpiredCredentials => "ExpiredToken",
            Self::InvalidOidcToken(_) => "InvalidIdentityToken",
            Self::RoleNotFound(_) => "AccessDenied",
            Self::BackendError(_) => "InternalError",
            Self::PreconditionFailed => "PreconditionFailed",
            Self::NotModified => "NotModified",
            Self::ConfigError(_) => "InternalError",
            Self::Internal(_) => "InternalError",
        }
    }

    /// HTTP status code for this error.
    pub fn status_code(&self) -> u16 {
        match self {
            Self::BucketNotFound(_) | Self::NoSuchKey(_) => 404,
            Self::AccessDenied | Self::MissingAuth | Self::ExpiredCredentials => 403,
            Self::SignatureDoesNotMatch => 403,
            Self::InvalidRequest(_) => 400,
            Self::InvalidOidcToken(_) => 400,
            Self::RoleNotFound(_) => 403,
            Self::PreconditionFailed => 412,
            Self::NotModified => 304,
            Self::BackendError(_) | Self::ConfigError(_) | Self::Internal(_) => 500,
        }
    }

    /// Return a message safe to show to external clients.
    ///
    /// For server-side errors (500), returns a generic message to avoid
    /// leaking backend infrastructure details. For client errors (4xx),
    /// returns the full message (the client already knows the bucket name,
    /// key, etc.).
    pub fn safe_message(&self) -> String {
        match self {
            Self::BackendError(_) | Self::ConfigError(_) | Self::Internal(_) => {
                "Internal server error".to_string()
            }
            other => other.to_string(),
        }
    }

    /// Convert an `object_store::Error` into a `ProxyError`.
    pub fn from_object_store_error(e: object_store::Error) -> Self {
        match e {
            object_store::Error::NotFound { path, .. } => Self::NoSuchKey(path),
            object_store::Error::Precondition { .. } => Self::PreconditionFailed,
            object_store::Error::NotModified { .. } => Self::NotModified,
            _ => Self::BackendError(e.to_string()),
        }
    }
}
