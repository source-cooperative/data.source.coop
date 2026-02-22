//! Server backend using reqwest for raw HTTP and default object_store connector.

use bytes::Bytes;
use futures::Stream;
use http::HeaderMap;
use object_store::ObjectStore;
use s3_proxy_core::backend::{build_object_store, ProxyBackend, RawResponse, RawStreamingResponse};
use s3_proxy_core::error::ProxyError;
use s3_proxy_core::types::BucketConfig;
use std::pin::Pin;
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
    type NativeBody = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

    fn create_store(&self, config: &BucketConfig) -> Result<Arc<dyn ObjectStore>, ProxyError> {
        build_object_store(config, |b| b)
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

    async fn send_streaming(
        &self,
        method: http::Method,
        url: String,
        headers: HeaderMap,
    ) -> Result<RawStreamingResponse<Self::NativeBody>, ProxyError> {
        tracing::debug!(
            method = %method,
            url = %url,
            "server: sending streaming backend request via reqwest"
        );

        let mut req_builder = self.client.request(method, &url);

        for (key, value) in headers.iter() {
            req_builder = req_builder.header(key, value);
        }

        let response = req_builder.send().await.map_err(|e| {
            tracing::error!(error = %e, "reqwest streaming request failed");
            ProxyError::BackendError(e.to_string())
        })?;

        let status = response.status().as_u16();
        let resp_headers = response.headers().clone();
        let body = Box::pin(response.bytes_stream());

        Ok(RawStreamingResponse {
            status,
            headers: resp_headers,
            body,
        })
    }
}
