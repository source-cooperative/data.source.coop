//! Response types for the Source Cooperative API (`/api/v1/...`), as fetched
//! and cached in [`crate::cache`] and resolved into multistore `BucketConfig`s
//! by [`crate::registry`].

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
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceProductList {
    pub products: Vec<SourceProduct>,
}
