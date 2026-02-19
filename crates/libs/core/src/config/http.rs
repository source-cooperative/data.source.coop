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
//! use s3_proxy_core::config::http::HttpProvider;
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
