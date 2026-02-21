# s3-proxy-server

Tokio/Hyper runtime for the S3 proxy gateway. This is the container-deployment crate — it wires the core library into a production HTTP server using native Rust async I/O.

## What This Crate Provides

A `ProxyBackend` implementation plus a server binary:

**`ServerBackend`** — implements `ProxyBackend`. Uses `object_store` with its default HTTP connector for high-level operations (GET, HEAD, PUT, LIST) and `reqwest` for raw multipart requests. GET responses stream from `object_store` through Hyper without buffering.

**`server::run()`** — starts a Hyper HTTP server that accepts connections and delegates to `ProxyHandler` with a `DefaultResolver`. Supports both path-style (`/bucket/key`) and virtual-hosted-style (`bucket.s3.example.com/key`) routing via the resolver's `virtual_host_domain` setting.

## Module Overview

```
src/
├── lib.rs           Crate root
├── body.rs          ProxyResponseBody → Hyper streaming response conversion
├── client.rs        ServerBackend implementing ProxyBackend
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
let backend = ServerBackend::new();
let handler = ProxyHandler::new(backend, MyResolver::new());

// Use handler.handle_request() in your Hyper service.
```

See `s3-proxy-cf-workers/src/source_resolver.rs` for a complete example.

## Streaming Behavior

For **GET** responses, `object_store` returns a `BoxStream<Bytes>` which is bridged to a Hyper streaming response body. Bytes flow through without buffering the entire object in memory.

For **HEAD** responses, only metadata is returned (empty body).

For **PUT** request bodies, the incoming Hyper body is collected to `Bytes` before passing to `object_store::put()`.

For **multipart uploads**, operations are sent as raw signed HTTP requests via `reqwest`. The `CompleteMultipartUpload` request body (a small XML manifest) is the only body the proxy fully reads and parses.
