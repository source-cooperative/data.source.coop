//! HTTP API-backed configuration provider.
//!
//! Fetches configuration from a centralized REST API. Useful when you have
//! a control plane service that manages proxy configuration.
//!
//! # Expected API Contract
//!
//! The API should expose:
//! - `GET /buckets` → `Vec<BucketConfig>`
//! - `GET /buckets/{name}` → `Option<BucketConfig>`
//! - `GET /roles/{role_id}` → `Option<RoleConfig>`
//! - `GET /credentials/{access_key_id}` → `Option<StoredCredential>`
//! - `POST /temporary-credentials` → stores a `TemporaryCredentials`
//! - `GET /temporary-credentials/{access_key_id}` → `Option<TemporaryCredentials>`
//!
//! # Example
//!
//! ```rust,ignore
//! use source_coop_core::config::http::HttpProvider;
//!
//! let provider = HttpProvider::new(
//!     "https://config-api.internal:8080".to_string(),
//!     Some("Bearer my-api-token".to_string()),
//! );
//! ```

use crate::config::ConfigProvider;
use crate::error::ProxyError;
use crate::types::{BucketConfig, RoleConfig, StoredCredential, TemporaryCredentials};
use std::sync::Arc;

/// Validate that a value is safe to use as a single URL path segment.
///
/// Rejects values containing `/`, `\`, `..`, null bytes, or that are empty,
/// to prevent path traversal against the config API.
fn validate_path_segment(value: &str, param_name: &str) -> Result<(), ProxyError> {
    if value.is_empty()
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
        || value == ".."
        || value == "."
    {
        return Err(ProxyError::InvalidRequest(format!(
            "invalid {}: contains illegal characters",
            param_name
        )));
    }
    Ok(())
}

/// Configuration provider that reads from a REST API.
#[derive(Clone)]
pub struct HttpProvider {
    inner: Arc<HttpProviderInner>,
}

struct HttpProviderInner {
    base_url: String,
    client: reqwest::Client,
    auth_header: Option<String>,
}

impl HttpProvider {
    /// Create a new HTTP config provider.
    ///
    /// `base_url`: The base URL of the config API (no trailing slash).
    /// `auth_header`: Optional Authorization header value (e.g., "Bearer ...").
    pub fn new(base_url: String, auth_header: Option<String>) -> Self {
        Self {
            inner: Arc::new(HttpProviderInner {
                base_url: base_url.trim_end_matches('/').to_string(),
                client: reqwest::Client::new(),
                auth_header,
            }),
        }
    }

    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        let mut req = self
            .inner
            .client
            .get(format!("{}{}", self.inner.base_url, path));
        if let Some(ref auth) = self.inner.auth_header {
            req = req.header("authorization", auth);
        }
        req
    }
}

impl ConfigProvider for HttpProvider {
    async fn list_buckets(&self) -> Result<Vec<BucketConfig>, ProxyError> {
        let resp = self
            .request("/buckets")
            .send()
            .await
            .map_err(|e| ProxyError::ConfigError(e.to_string()))?;

        resp.json()
            .await
            .map_err(|e| ProxyError::ConfigError(e.to_string()))
    }

    async fn get_bucket(&self, name: &str) -> Result<Option<BucketConfig>, ProxyError> {
        validate_path_segment(name, "bucket name")?;
        let resp = self
            .request(&format!("/buckets/{}", name))
            .send()
            .await
            .map_err(|e| ProxyError::ConfigError(e.to_string()))?;

        if resp.status().as_u16() == 404 {
            return Ok(None);
        }

        resp.json()
            .await
            .map(Some)
            .map_err(|e| ProxyError::ConfigError(e.to_string()))
    }

    async fn get_role(&self, role_id: &str) -> Result<Option<RoleConfig>, ProxyError> {
        validate_path_segment(role_id, "role ID")?;
        let resp = self
            .request(&format!("/roles/{}", role_id))
            .send()
            .await
            .map_err(|e| ProxyError::ConfigError(e.to_string()))?;

        if resp.status().as_u16() == 404 {
            return Ok(None);
        }

        resp.json()
            .await
            .map(Some)
            .map_err(|e| ProxyError::ConfigError(e.to_string()))
    }

    async fn get_credential(
        &self,
        access_key_id: &str,
    ) -> Result<Option<StoredCredential>, ProxyError> {
        validate_path_segment(access_key_id, "access key ID")?;
        let resp = self
            .request(&format!("/credentials/{}", access_key_id))
            .send()
            .await
            .map_err(|e| ProxyError::ConfigError(e.to_string()))?;

        if resp.status().as_u16() == 404 {
            return Ok(None);
        }

        resp.json()
            .await
            .map(Some)
            .map_err(|e| ProxyError::ConfigError(e.to_string()))
    }

    async fn store_temporary_credential(
        &self,
        cred: &TemporaryCredentials,
    ) -> Result<(), ProxyError> {
        let mut req = self
            .inner
            .client
            .post(format!("{}/temporary-credentials", self.inner.base_url))
            .json(cred);

        if let Some(ref auth) = self.inner.auth_header {
            req = req.header("authorization", auth);
        }

        req.send()
            .await
            .map_err(|e| ProxyError::ConfigError(e.to_string()))?;

        Ok(())
    }

    async fn get_temporary_credential(
        &self,
        access_key_id: &str,
    ) -> Result<Option<TemporaryCredentials>, ProxyError> {
        validate_path_segment(access_key_id, "access key ID")?;
        let resp = self
            .request(&format!("/temporary-credentials/{}", access_key_id))
            .send()
            .await
            .map_err(|e| ProxyError::ConfigError(e.to_string()))?;

        if resp.status().as_u16() == 404 {
            return Ok(None);
        }

        resp.json()
            .await
            .map(Some)
            .map_err(|e| ProxyError::ConfigError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_path_segment_rejects_traversal() {
        assert!(validate_path_segment("../admin", "test").is_err());
        assert!(validate_path_segment("foo/bar", "test").is_err());
        assert!(validate_path_segment("foo\\bar", "test").is_err());
        assert!(validate_path_segment("..", "test").is_err());
        assert!(validate_path_segment(".", "test").is_err());
        assert!(validate_path_segment("", "test").is_err());
        assert!(validate_path_segment("foo\0bar", "test").is_err());
    }

    #[test]
    fn validate_path_segment_accepts_normal_values() {
        assert!(validate_path_segment("my-bucket", "test").is_ok());
        assert!(validate_path_segment("AKIAIOSFODNN7EXAMPLE", "test").is_ok());
        assert!(validate_path_segment("role-123_abc", "test").is_ok());
        assert!(validate_path_segment("bucket.with.dots", "test").is_ok());
    }
}
