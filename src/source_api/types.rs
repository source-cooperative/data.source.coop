//! Response types for the Source Cooperative API (`/api/v1/...`), as fetched
//! and cached in [`super::cache`] and resolved into multistore `BucketConfig`s
//! by [`super::registry`].

use multistore::error::ProxyError;
use serde::Deserialize;
use std::collections::HashMap;

use crate::backend_auth::BackendAuth;

/// Product visibility, mirroring `ProductVisibility` in the source.coop data
/// model. Replaced the legacy `data_mode` field in source.coop#284. Only
/// `Public` is acted on; every other value (`unlisted`, `restricted`, missing,
/// or unrecognized) deserializes to `Unknown` and is treated as non-public, so
/// we fail closed.
#[derive(Debug, Default, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceProduct {
    pub product_id: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub visibility: Visibility,
    pub metadata: SourceProductMetadata,
}

impl SourceProduct {
    pub fn is_public(&self) -> bool {
        !self.disabled && self.visibility == Visibility::Public
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceProductMetadata {
    pub mirrors: HashMap<String, SourceProductMirror>,
    pub primary_mirror: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceProductMirror {
    pub connection_id: String,
    pub prefix: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DataConnection {
    pub data_connection_id: String,
    /// Whether the connection forbids writes. Required (no serde default): an
    /// absent flag fails the fetch rather than defaulting to writable.
    pub read_only: bool,
    pub details: DataConnectionDetails,
    /// How the proxy authenticates to this connection's backend. A sibling of
    /// `details`, matching the Source API's `DataConnection` shape. Absent →
    /// [`BackendAuth::Unsigned`] (public bucket); a present-but-malformed value
    /// becomes `Unsupported` (fail closed) rather than erroring the fetch (see
    /// [`deserialize_lenient`](crate::backend_auth::deserialize_lenient)).
    #[serde(default, deserialize_with = "crate::backend_auth::deserialize_lenient")]
    pub authentication: BackendAuth,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DataConnectionDetails {
    pub provider: String,
    pub bucket: Option<String>,
    pub region: Option<String>,
    pub base_prefix: Option<String>,
    pub account_name: Option<String>,
    pub container_name: Option<String>,
    /// Explicit S3 endpoint for a non-AWS, S3-compatible backend (Cloudflare
    /// R2, MinIO, Ceph). When absent the endpoint is derived from `region`.
    pub endpoint: Option<String>,
}

impl DataConnectionDetails {
    /// Map the raw `provider` to its multistore `backend_type` (`s3`/`az`/`gcs`)
    /// and the provider-specific `backend_options`. Doing the provider match
    /// once keeps the type and its options in a single source of truth. The GCS
    /// arm sets `bucket_name` (multistore's GCS store requires it, same as the
    /// s3 arm).
    pub fn backend_options(&self) -> Result<(String, HashMap<String, String>), ProxyError> {
        let mut options = HashMap::new();
        let backend_type = match self.provider.as_str() {
            "s3" => {
                if let Some(ref bucket) = self.bucket {
                    options.insert("bucket_name".to_string(), bucket.clone());
                }
                if let Some(ref region) = self.region {
                    options.insert("region".to_string(), region.clone());
                }
                // An explicit endpoint wins: S3-compatible backends (R2 and
                // friends) carry `region: "auto"`, which derives the
                // unresolvable `https://s3.auto.amazonaws.com` and fails the
                // backend fetch at DNS (Cloudflare 1016 → a 530 to the client).
                if let Some(endpoint) = self.endpoint.clone().or_else(|| {
                    self.region
                        .as_ref()
                        .map(|region| format!("https://s3.{}.amazonaws.com", region))
                }) {
                    options.insert("endpoint".to_string(), endpoint);
                }
                "s3"
            }
            "az" | "azure" => {
                if let Some(ref account_name) = self.account_name {
                    options.insert("account_name".to_string(), account_name.clone());
                }
                if let Some(ref container) = self.container_name {
                    options.insert("container_name".to_string(), container.clone());
                }
                "az"
            }
            "gcs" | "gs" => {
                if let Some(ref bucket) = self.bucket {
                    options.insert("bucket_name".to_string(), bucket.clone());
                }
                "gcs"
            }
            other => {
                return Err(ProxyError::Internal(format!(
                    "unsupported provider: {}",
                    other
                )))
            }
        }
        .to_string();
        Ok((backend_type, options))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceProductList {
    pub products: Vec<SourceProduct>,
}
