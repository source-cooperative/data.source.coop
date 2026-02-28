//! Static file-based configuration provider.
//!
//! Loads configuration from a TOML or JSON file at startup.
//! Suitable for simple deployments or development.

use crate::config::ConfigProvider;
use crate::error::ProxyError;
use crate::types::{BucketConfig, RoleConfig, StoredCredential};
use serde::Deserialize;
use std::sync::Arc;

/// Full configuration file structure.
#[derive(Debug, Clone, Deserialize)]
pub struct StaticConfig {
    #[serde(default)]
    pub buckets: Vec<BucketConfig>,
    #[serde(default)]
    pub roles: Vec<RoleConfig>,
    #[serde(default)]
    pub credentials: Vec<StoredCredential>,
}

/// Configuration provider backed by a static TOML/JSON file.
///
/// # Example
///
/// ```rust,ignore
/// let provider = StaticProvider::from_toml(r#"
///     [[buckets]]
///     name = "public-data"
///     backend_type = "s3"
///     anonymous_access = true
///     allowed_roles = []
///
///     [buckets.backend_options]
///     endpoint = "https://s3.amazonaws.com"
///     bucket_name = "my-real-bucket"
///     region = "us-east-1"
///     access_key_id = "AKIA..."
///     secret_access_key = "..."
/// "#)?;
/// ```
#[derive(Clone)]
pub struct StaticProvider {
    inner: Arc<StaticProviderInner>,
}

struct StaticProviderInner {
    config: StaticConfig,
}

impl StaticProvider {
    /// Parse a TOML string into a provider.
    pub fn from_toml(toml_str: &str) -> Result<Self, ProxyError> {
        let config: StaticConfig =
            toml::from_str(toml_str).map_err(|e| ProxyError::ConfigError(e.to_string()))?;
        Ok(Self::from_config(config))
    }

    /// Parse a JSON string into a provider.
    pub fn from_json(json_str: &str) -> Result<Self, ProxyError> {
        let config: StaticConfig =
            serde_json::from_str(json_str).map_err(|e| ProxyError::ConfigError(e.to_string()))?;
        Ok(Self::from_config(config))
    }

    /// Read and parse a TOML file.
    pub fn from_file(path: &str) -> Result<Self, ProxyError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| ProxyError::ConfigError(e.to_string()))?;
        if path.ends_with(".json") {
            Self::from_json(&content)
        } else {
            Self::from_toml(&content)
        }
    }

    pub fn from_config(config: StaticConfig) -> Self {
        Self {
            inner: Arc::new(StaticProviderInner { config }),
        }
    }
}

impl ConfigProvider for StaticProvider {
    async fn list_buckets(&self) -> Result<Vec<BucketConfig>, ProxyError> {
        Ok(self.inner.config.buckets.clone())
    }

    async fn get_bucket(&self, name: &str) -> Result<Option<BucketConfig>, ProxyError> {
        Ok(self
            .inner
            .config
            .buckets
            .iter()
            .find(|b| b.name == name)
            .cloned())
    }

    async fn get_role(&self, role_id: &str) -> Result<Option<RoleConfig>, ProxyError> {
        Ok(self
            .inner
            .config
            .roles
            .iter()
            .find(|r| r.role_id == role_id)
            .cloned())
    }

    async fn get_credential(
        &self,
        access_key_id: &str,
    ) -> Result<Option<StoredCredential>, ProxyError> {
        Ok(self
            .inner
            .config
            .credentials
            .iter()
            .find(|c| c.access_key_id == access_key_id)
            .cloned())
    }
}
