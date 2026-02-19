# s3-proxy-server

Tokio/Hyper runtime for the S3 proxy gateway. This is the container-deployment crate — it wires the core library into a production HTTP server using native Rust async I/O.

## What This Crate Provides

Three concrete implementations of core traits, plus a server binary:

**`ServerBody`** — implements `BodyStream` using `http-body-util`. Wraps `Full<Bytes>`, `Empty<Bytes>`, or a streaming `reqwest::Response` for backend responses. Backend response bodies remain as reqwest's streaming type until consumed, avoiding unnecessary buffering.

**`ReqwestBackendClient`** — implements `BackendClient` using `reqwest`. Sends signed requests to backing object stores with connection pooling (`pool_max_idle_per_host = 20`).

**`server::run()`** — starts a Hyper HTTP server that accepts connections and delegates to `ProxyHandler` with a `DefaultResolver`. Supports both path-style (`/bucket/key`) and virtual-hosted-style (`bucket.s3.example.com/key`) routing via the resolver's `virtual_host_domain` setting.

## Module Overview

```
src/
├── lib.rs           Crate root
├── body.rs          ServerBody implementing BodyStream
├── client.rs        ReqwestBackendClient implementing BackendClient
├── server.rs        Hyper server setup, request routing
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
let client = ReqwestBackendClient::new();
let handler = ProxyHandler::new(client, MyResolver::new());

// Use handler.handle_request() in your Hyper service.
```

See `s3-proxy-cf-workers/src/source_resolver.rs` for a complete example.

## Streaming Behavior

For **GET/HEAD** responses, the backend response body stays as a `reqwest::Response` (streaming) and is forwarded to the client. The proxy does not buffer the full object in memory.

For **PUT** request bodies, the current implementation collects the incoming body before forwarding. A follow-up optimization would pipe the incoming Hyper body directly to the reqwest request body using `reqwest::Body::wrap_stream`.

For **multipart uploads**, each part is individually streamed. The `CompleteMultipartUpload` request body (a small XML manifest) is the only body the proxy fully reads and parses.
