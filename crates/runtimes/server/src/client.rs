//! Backend client using reqwest for outbound HTTP requests.

use crate::body::ServerBody;
use s3_proxy_core::backend::{BackendClient, BackendRequest, BackendResponse};
use s3_proxy_core::error::ProxyError;

/// Backend client that uses `reqwest` to make outbound requests.
///
/// This keeps the response body as a `reqwest::Response` which can be
/// streamed back to the client without buffering.
#[derive(Clone)]
pub struct ReqwestBackendClient {
    client: reqwest::Client,
}

impl ReqwestBackendClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .pool_max_idle_per_host(20)
                .build()
                .expect("failed to build reqwest client"),
        }
    }
}

impl Default for ReqwestBackendClient {
    fn default() -> Self {
        Self::new()
    }
}

impl BackendClient for ReqwestBackendClient {
    type Body = ServerBody;

    async fn send_request(
        &self,
        request: BackendRequest<ServerBody>,
    ) -> Result<BackendResponse<ServerBody>, ProxyError> {
        tracing::debug!(
            method = %request.method,
            url = %request.url,
            "server: sending backend request via reqwest"
        );

        let mut req_builder = self.client.request(request.method, &request.url);

        // Set headers
        for (key, value) in request.headers.iter() {
            req_builder = req_builder.header(key, value);
        }

        // Set body
        req_builder = match request.body {
            ServerBody::Full(full) => {
                use http_body_util::BodyExt;
                let bytes = full
                    .collect()
                    .await
                    .map_err(|e| ProxyError::BackendError(e.to_string()))?
                    .to_bytes();
                req_builder.body(bytes)
            }
            ServerBody::Empty(_) => req_builder,
            ServerBody::Streaming(resp) => {
                let bytes = resp
                    .bytes()
                    .await
                    .map_err(|e| ProxyError::BackendError(e.to_string()))?;
                req_builder.body(bytes)
            }
        };

        let response = req_builder
            .send()
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "reqwest backend request failed");
                ProxyError::BackendError(e.to_string())
            })?;

        let status = response.status().as_u16();
        let headers = response.headers().clone();

        tracing::debug!(status = status, "server: backend response received");

        Ok(BackendResponse {
            status,
            headers,
            body: ServerBody::Streaming(response),
        })
    }
}
