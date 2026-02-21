//! Server backend using reqwest for raw HTTP and default object_store connector.

use bytes::Bytes;
use http::HeaderMap;
use object_store::aws::AmazonS3Builder;
use object_store::ObjectStore;
use s3_proxy_core::backend::{ProxyBackend, RawResponse};
use s3_proxy_core::error::ProxyError;
use s3_proxy_core::types::BucketConfig;
use std::sync::Arc;

/// Backend for the Tokio/Hyper server runtime.
///
/// Uses reqwest for raw HTTP (multipart operations) and the default
/// object_store HTTP connector for high-level operations.
#[derive(Clone)]
pub struct ServerBackend {
    client: reqwest::Client,
}

impl ServerBackend {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .pool_max_idle_per_host(20)
                .build()
                .expect("failed to build reqwest client"),
        }
    }
}

impl Default for ServerBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ProxyBackend for ServerBackend {
    fn create_store(&self, config: &BucketConfig) -> Result<Arc<dyn ObjectStore>, ProxyError> {
        let mut builder = AmazonS3Builder::new()
            .with_endpoint(&config.backend_endpoint)
            .with_bucket_name(&config.backend_bucket)
            .with_region(&config.backend_region);

        if !config.backend_access_key_id.is_empty() {
            builder = builder
                .with_access_key_id(&config.backend_access_key_id)
                .with_secret_access_key(&config.backend_secret_access_key);
        } else {
            builder = builder.with_skip_signature(true);
        }

        Ok(Arc::new(
            builder
                .build()
                .map_err(|e| ProxyError::ConfigError(format!("failed to build S3 store: {}", e)))?,
        ))
    }

    async fn send_raw(
        &self,
        method: http::Method,
        url: String,
        headers: HeaderMap,
        body: Bytes,
    ) -> Result<RawResponse, ProxyError> {
        tracing::debug!(
            method = %method,
            url = %url,
            "server: sending raw backend request via reqwest"
        );

        let mut req_builder = self.client.request(method, &url);

        for (key, value) in headers.iter() {
            req_builder = req_builder.header(key, value);
        }

        if !body.is_empty() {
            req_builder = req_builder.body(body);
        }

        let response = req_builder.send().await.map_err(|e| {
            tracing::error!(error = %e, "reqwest raw request failed");
            ProxyError::BackendError(e.to_string())
        })?;

        let status = response.status().as_u16();
        let resp_headers = response.headers().clone();
        let resp_body = response.bytes().await.map_err(|e| {
            ProxyError::BackendError(format!("failed to read raw response body: {}", e))
        })?;

        Ok(RawResponse {
            status,
            headers: resp_headers,
            body: resp_body,
        })
    }
}
