//! Shared types used across the proxy.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for a virtual bucket exposed by the proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BucketConfig {
    /// The virtual bucket name exposed to clients.
    pub name: String,

    /// Provider type: "s3", "az", "gcs", etc.
    pub backend_type: String,

    /// Optional prefix to prepend to all keys when forwarding.
    pub backend_prefix: Option<String>,

    /// Whether this bucket allows anonymous (unsigned) access.
    pub anonymous_access: bool,

    /// IAM role ARNs that are allowed to access this bucket.
    /// Empty means only anonymous access (if enabled) or long-lived credentials.
    pub allowed_roles: Vec<String>,

    /// Provider-specific config passed to the object_store builder.
    /// Keys are the short aliases accepted by each provider's ConfigKey::from_str().
    /// S3: "endpoint", "bucket_name", "region", "access_key_id", "secret_access_key", "skip_signature"
    /// Azure: "account_name", "container_name", "access_key", "skip_signature"
    /// GCS: "bucket_name", "service_account_key", "skip_signature"
    #[serde(default)]
    pub backend_options: HashMap<String, String>,
}

/// Known backend provider types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    S3,
    Azure,
    Gcs,
}

impl BucketConfig {
    /// Parse the `backend_type` string into a known [`BackendType`].
    pub fn parsed_backend_type(&self) -> Option<BackendType> {
        match self.backend_type.as_str() {
            "s3" => Some(BackendType::S3),
            "az" | "azure" => Some(BackendType::Azure),
            "gcs" | "gs" => Some(BackendType::Gcs),
            _ => None,
        }
    }

    /// Whether this backend supports S3-style multipart uploads via raw HTTP.
    pub fn supports_s3_multipart(&self) -> bool {
        matches!(self.parsed_backend_type(), Some(BackendType::S3))
    }

    /// Look up a value in `backend_options`.
    pub fn option(&self, key: &str) -> Option<&str> {
        self.backend_options.get(key).map(|s| s.as_str())
    }
}

/// Configuration for an IAM role that can be assumed via STS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleConfig {
    /// The role identifier (used as the RoleArn in AssumeRoleWithWebIdentity).
    pub role_id: String,

    /// Human-readable name.
    pub name: String,

    /// OIDC provider URLs trusted by this role (e.g., "https://token.actions.githubusercontent.com").
    pub trusted_oidc_issuers: Vec<String>,

    /// Required audience claim value.
    pub required_audience: Option<String>,

    /// Conditions on the subject claim (glob patterns).
    /// e.g., "repo:myorg/myrepo:ref:refs/heads/main"
    pub subject_conditions: Vec<String>,

    /// Buckets and prefixes this role can access.
    pub allowed_scopes: Vec<AccessScope>,

    /// Maximum session duration in seconds.
    pub max_session_duration_secs: u64,
}

/// Defines what a credential is allowed to access.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessScope {
    pub bucket: String,
    /// Allowed key prefixes. Empty means full bucket access.
    pub prefixes: Vec<String>,
    /// Allowed actions.
    pub actions: Vec<Action>,
}

/// S3 actions that can be authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    GetObject,
    HeadObject,
    PutObject,
    ListBucket,
    CreateMultipartUpload,
    UploadPart,
    CompleteMultipartUpload,
    AbortMultipartUpload,
    DeleteObject,
}

/// A long-lived access credential stored in the config backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCredential {
    pub access_key_id: String,
    /// This is the HMAC signing key, not stored in plaintext ideally.
    pub secret_access_key: String,
    pub principal_name: String,
    pub allowed_scopes: Vec<AccessScope>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub enabled: bool,
}

/// Temporary credentials minted by the STS API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporaryCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: String,
    pub expiration: DateTime<Utc>,
    pub allowed_scopes: Vec<AccessScope>,
    pub assumed_role_id: String,
    pub source_identity: String,
}

/// Represents the resolved identity after authentication.
#[derive(Debug, Clone)]
pub enum ResolvedIdentity {
    Anonymous,
    LongLived {
        credential: StoredCredential,
    },
    Temporary {
        credentials: TemporaryCredentials,
    },
}

/// The parsed S3 operation extracted from an incoming request.
#[derive(Debug, Clone)]
pub enum S3Operation {
    GetObject {
        bucket: String,
        key: String,
    },
    HeadObject {
        bucket: String,
        key: String,
    },
    PutObject {
        bucket: String,
        key: String,
    },
    CreateMultipartUpload {
        bucket: String,
        key: String,
    },
    UploadPart {
        bucket: String,
        key: String,
        upload_id: String,
        part_number: u32,
    },
    CompleteMultipartUpload {
        bucket: String,
        key: String,
        upload_id: String,
    },
    AbortMultipartUpload {
        bucket: String,
        key: String,
        upload_id: String,
    },
    DeleteObject {
        bucket: String,
        key: String,
    },
    ListBucket {
        bucket: String,
        /// Raw query string from the incoming request, forwarded to the backend.
        /// The proxy may modify `prefix` (prepend backend_prefix) and inject
        /// defaults for `max-keys` and `list-type`.
        raw_query: Option<String>,
    },
    /// List all virtual buckets exposed by the proxy.
    ListBuckets,
    /// STS AssumeRoleWithWebIdentity (served on the same endpoint).
    AssumeRoleWithWebIdentity {
        role_arn: String,
        web_identity_token: String,
        duration_seconds: Option<u64>,
    },
}

impl S3Operation {
    /// The authorization action for this operation.
    pub fn action(&self) -> Action {
        match self {
            S3Operation::GetObject { .. } => Action::GetObject,
            S3Operation::HeadObject { .. } => Action::HeadObject,
            S3Operation::PutObject { .. } => Action::PutObject,
            S3Operation::ListBucket { .. } => Action::ListBucket,
            S3Operation::CreateMultipartUpload { .. } => Action::CreateMultipartUpload,
            S3Operation::UploadPart { .. } => Action::UploadPart,
            S3Operation::CompleteMultipartUpload { .. } => Action::CompleteMultipartUpload,
            S3Operation::AbortMultipartUpload { .. } => Action::AbortMultipartUpload,
            S3Operation::DeleteObject { .. } => Action::DeleteObject,
            S3Operation::ListBuckets => Action::ListBucket,
            S3Operation::AssumeRoleWithWebIdentity { .. } => Action::GetObject, // STS is handled separately
        }
    }

    /// The bucket name, if any.
    pub fn bucket(&self) -> Option<&str> {
        match self {
            S3Operation::GetObject { bucket, .. }
            | S3Operation::HeadObject { bucket, .. }
            | S3Operation::PutObject { bucket, .. }
            | S3Operation::ListBucket { bucket, .. }
            | S3Operation::CreateMultipartUpload { bucket, .. }
            | S3Operation::UploadPart { bucket, .. }
            | S3Operation::CompleteMultipartUpload { bucket, .. }
            | S3Operation::AbortMultipartUpload { bucket, .. }
            | S3Operation::DeleteObject { bucket, .. } => Some(bucket),
            S3Operation::ListBuckets => None,
            S3Operation::AssumeRoleWithWebIdentity { .. } => None,
        }
    }

    /// The object key, if any. Returns empty string for non-object operations.
    pub fn key(&self) -> &str {
        match self {
            S3Operation::GetObject { key, .. }
            | S3Operation::HeadObject { key, .. }
            | S3Operation::PutObject { key, .. }
            | S3Operation::CreateMultipartUpload { key, .. }
            | S3Operation::UploadPart { key, .. }
            | S3Operation::CompleteMultipartUpload { key, .. }
            | S3Operation::AbortMultipartUpload { key, .. }
            | S3Operation::DeleteObject { key, .. } => key,
            S3Operation::ListBucket { .. }
            | S3Operation::ListBuckets
            | S3Operation::AssumeRoleWithWebIdentity { .. } => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action() {
        let op = S3Operation::GetObject {
            bucket: "b".into(),
            key: "k".into(),
        };
        assert_eq!(op.action(), Action::GetObject);

        let op = S3Operation::PutObject {
            bucket: "b".into(),
            key: "k".into(),
        };
        assert_eq!(op.action(), Action::PutObject);

        let op = S3Operation::ListBucket {
            bucket: "b".into(),
            raw_query: None,
        };
        assert_eq!(op.action(), Action::ListBucket);

        assert_eq!(S3Operation::ListBuckets.action(), Action::ListBucket);

        let op = S3Operation::DeleteObject {
            bucket: "b".into(),
            key: "k".into(),
        };
        assert_eq!(op.action(), Action::DeleteObject);
    }

    #[test]
    fn test_bucket() {
        let op = S3Operation::GetObject {
            bucket: "my-bucket".into(),
            key: "k".into(),
        };
        assert_eq!(op.bucket(), Some("my-bucket"));

        assert_eq!(S3Operation::ListBuckets.bucket(), None);

        let op = S3Operation::AssumeRoleWithWebIdentity {
            role_arn: "arn".into(),
            web_identity_token: "tok".into(),
            duration_seconds: None,
        };
        assert_eq!(op.bucket(), None);
    }

    #[test]
    fn test_key() {
        let op = S3Operation::GetObject {
            bucket: "b".into(),
            key: "my/key.txt".into(),
        };
        assert_eq!(op.key(), "my/key.txt");

        let op = S3Operation::ListBucket {
            bucket: "b".into(),
            raw_query: Some("prefix=foo/".into()),
        };
        assert_eq!(op.key(), "");

        assert_eq!(S3Operation::ListBuckets.key(), "");
    }
}
