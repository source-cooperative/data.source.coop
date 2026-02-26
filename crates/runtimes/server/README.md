# s3-proxy-server

Tokio/Hyper runtime for the S3 proxy gateway. This is the container-deployment crate — it wires the core library into a production HTTP server using native Rust async I/O.

## What This Crate Provides

A `ProxyBackend` implementation plus a server binary:

**`ServerBackend`** — implements `ProxyBackend`. Provides `create_signer()` for presigned URL generation (GET/HEAD/PUT/DELETE), `create_store()` for LIST operations, and `send_raw()` via reqwest for multipart uploads. All Forward operations (GET/HEAD/PUT/DELETE) execute presigned URLs via reqwest; GET response bodies and PUT request bodies stream without buffering.

**`server::run()`** — starts a Hyper HTTP server that accepts connections and delegates to `ProxyHandler` with a `DefaultResolver`. Supports both path-style (`/bucket/key`) and virtual-hosted-style (`bucket.s3.example.com/key`) routing via the resolver's `virtual_host_domain` setting.

## Module Overview

```
src/
├── lib.rs           Crate root
├── body.rs          ProxyResult → Hyper response conversion (Bytes/Empty only)
├── client.rs        ServerBackend implementing ProxyBackend
├── server.rs        Hyper server setup, two-phase request handling, Forward execution
└── bin/
    └── s3-proxy.rs  CLI binary entry point
```

## Binary Usage

```bash
cargo build --release -p s3-proxy-server

# Minimal
./target/release/s3-proxy --config config.toml

# Full options
./target/release/s3-proxy \
    --config /etc/s3-proxy/config.toml \
    --listen 0.0.0.0:9000 \
    --domain s3.local

# Environment variable for log level
RUST_LOG=s3_proxy=debug ./target/release/s3-proxy --config config.toml
```

## Docker

```bash
docker build -t s3-proxy .
docker run -v ./config.toml:/etc/s3-proxy/config.toml -p 8080:8080 s3-proxy
```

## Using a Different Config Provider

The default binary uses `StaticProvider` (TOML file) wrapped in `CachedProvider`. The `run()` function accepts any `ConfigProvider` and wraps it in a `DefaultResolver` internally. To use a different provider, modify the binary or write your own:

```rust
use s3_proxy_core::config::cached::CachedProvider;
use s3_proxy_core::config::http::HttpProvider;  // requires config-http feature
use s3_proxy_server::server::{run, ServerConfig};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = HttpProvider::new(
        "https://config-api.internal:8080".into(),
        Some("Bearer my-token".into()),
    );
    let config = CachedProvider::new(base, Duration::from_secs(300));

    run(config, ServerConfig::default()).await
}
```

## Using a Custom Request Resolver

For full control over request routing and authorization, you can bypass `run()` and wire up a `ProxyHandler` with a custom `RequestResolver` directly. This is useful when your URL namespace doesn't follow the standard S3 bucket/key pattern, or when authorization is handled by an external service.

```rust
use s3_proxy_core::proxy::ProxyHandler;
use s3_proxy_core::resolver::{RequestResolver, ResolvedAction};
use s3_proxy_core::error::ProxyError;

#[derive(Clone)]
struct MyResolver { /* ... */ }

impl RequestResolver for MyResolver {
    async fn resolve(
        &self,
        method: &http::Method,
        path: &str,
        query: Option<&str>,
        headers: &http::HeaderMap,
    ) -> Result<ResolvedAction, ProxyError> {
        // Custom routing: parse the URL, authenticate, authorize,
        // and return a ResolvedAction::Proxy or ResolvedAction::Response.
        todo!()
    }
}

// Then create the handler directly:
let backend = ServerBackend::new();
let handler = ProxyHandler::new(backend, MyResolver::new());

// Use handler.resolve_request() in your Hyper service — returns HandlerAction.
```

See `crates/libs/source-coop/src/resolver.rs` for a complete example.

## Streaming Behavior

For **GET** responses, the handler generates a presigned URL and returns a `Forward` action. The server executes the URL via reqwest and streams the response body through Hyper using `bytes_stream()` — no buffering.

For **PUT** requests, the handler generates a presigned URL and returns a `Forward` action. The server streams the Hyper `Incoming` body directly to the presigned URL via `reqwest::Body::wrap_stream()` — no body materialization.

For **HEAD/DELETE** responses, the handler generates a presigned URL. The server executes it and returns the status + headers.

For **LIST** responses, `object_store` handles the request internally and the handler returns a `Response` with XML body.

For **multipart uploads**, operations are sent as raw signed HTTP requests via `reqwest`. The request body is materialized to `Bytes` first (multipart XML payloads are small).
