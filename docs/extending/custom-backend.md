# Custom Backend

The `ProxyBackend` trait abstracts runtime-specific I/O. Implement it when deploying to a platform that's neither a standard server nor Cloudflare Workers.

## The Trait

```rust
use source_coop_core::backend::ProxyBackend;
use source_coop_core::types::BucketConfig;
use source_coop_core::error::ProxyError;
use object_store::{ObjectStore, signer::Signer};
use std::sync::Arc;

pub trait ProxyBackend: Clone + MaybeSend + MaybeSync + 'static {
    /// Create an ObjectStore for LIST operations
    fn create_store(&self, config: &BucketConfig) -> Result<Arc<dyn ObjectStore>, ProxyError>;

    /// Create a Signer for presigned URL generation (GET/HEAD/PUT/DELETE)
    fn create_signer(&self, config: &BucketConfig) -> Result<Arc<dyn Signer>, ProxyError>;

    /// Send a pre-signed HTTP request (multipart operations)
    fn send_raw(
        &self,
        method: http::Method,
        url: String,
        headers: HeaderMap,
        body: Bytes,
    ) -> impl Future<Output = Result<RawResponse, ProxyError>> + MaybeSend;
}
```

## Three Responsibilities

### `create_store()`

Returns an `Arc<dyn ObjectStore>` used only for LIST operations. The runtime may need to inject a custom HTTP connector:

```rust
fn create_store(&self, config: &BucketConfig) -> Result<Arc<dyn ObjectStore>, ProxyError> {
    // Use the shared helper, optionally injecting a custom connector
    build_object_store(config, |builder| {
        match builder {
            StoreBuilder::S3(s) => StoreBuilder::S3(s.with_http_connector(MyConnector)),
            other => other,
        }
    })
}
```

### `create_signer()`

Returns an `Arc<dyn Signer>` for generating presigned URLs. Signing is pure computation — no HTTP connector needed:

```rust
fn create_signer(&self, config: &BucketConfig) -> Result<Arc<dyn Signer>, ProxyError> {
    build_signer(config)
}
```

### `send_raw()`

Executes a pre-signed HTTP request for multipart operations. Use your platform's HTTP client:

```rust
async fn send_raw(
    &self,
    method: http::Method,
    url: String,
    headers: HeaderMap,
    body: Bytes,
) -> Result<RawResponse, ProxyError> {
    let response = self.http_client
        .request(method, &url)
        .headers(headers)
        .body(body)
        .send()
        .await
        .map_err(|e| ProxyError::BackendError(e.to_string()))?;

    Ok(RawResponse {
        status: response.status(),
        headers: response.headers().clone(),
        body: response.bytes().await
            .map_err(|e| ProxyError::BackendError(e.to_string()))?,
    })
}
```

## Helper Functions

The `backend` module provides shared helpers:

- **`build_object_store(config, connector_fn)`** — Dispatches on `backend_type` ("s3", "az", "gcs"), iterates `backend_options` with `with_config()`, and applies the connector function
- **`build_signer(config)`** — Returns the appropriate signer: `object_store`'s built-in signer for authenticated backends, or `UnsignedUrlSigner` for anonymous backends

These handle the multi-provider dispatch logic so your backend implementation only needs to provide the HTTP transport layer.

## Wiring Into the Handler

```rust
let backend = MyBackend::new(http_client);
let resolver = DefaultResolver::new(config_provider, token_key, domain);
let handler = ProxyHandler::new(backend, resolver);

// In your request handler, handle all three action types:
match handler.resolve_request(method, path, query, &headers).await {
    HandlerAction::Forward(fwd) => {
        // Execute presigned URL with your HTTP client
        // Stream request body (PUT) or response body (GET)
    }
    HandlerAction::Response(res) => {
        // Return the complete response (LIST, errors)
    }
    HandlerAction::NeedsBody(pending) => {
        // Collect request body, then:
        let result = handler.handle_with_body(pending, body).await;
        // Return the result
    }
}
```
