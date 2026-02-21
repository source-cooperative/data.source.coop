//! Shared types used across the proxy.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Configuration for a virtual bucket exposed by the proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BucketConfig {
    /// The virtual bucket name exposed to clients.
    pub name: String,

    /// The backing object store endpoint (e.g., "https://s3.amazonaws.com").
    pub backend_endpoint: String,

    /// The real bucket name on the backing store.
    pub backend_bucket: String,

    /// Optional prefix to prepend to all keys when forwarding.
    pub backend_prefix: Option<String>,

    /// The region to use when signing requests to the backend.
    pub backend_region: String,

    /// Credentials for signing outbound requests to the backing store.
    pub backend_access_key_id: String,
    pub backend_secret_access_key: String,

    /// Whether this bucket allows anonymous (unsigned) access.
    pub anonymous_access: bool,

    /// IAM role ARNs that are allowed to access this bucket.
    /// Empty means only anonymous access (if enabled) or long-lived credentials.
    pub allowed_roles: Vec<String>,
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
