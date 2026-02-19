# s3-proxy-core

Runtime-agnostic core library for the S3 proxy gateway. This crate contains all business logic — S3 request parsing, SigV4 signing/verification, authorization, configuration retrieval, and the proxy handler — without depending on any async runtime.

## Why This Crate Exists Separately

The proxy needs to run on fundamentally different runtimes: Tokio/Hyper in containers and Cloudflare Workers on the edge. These runtimes have incompatible stream types, HTTP primitives, and threading models (multi-threaded vs single-threaded WASM). By keeping the core free of runtime dependencies, it compiles cleanly to both `x86_64-unknown-linux-gnu` and `wasm32-unknown-unknown`.

## Key Abstractions

The core defines four trait boundaries that runtime crates implement:

**`BodyStream`** — A streaming body type. The core almost never reads body bytes; it passes them through opaquely from client to backend and back. Each runtime provides its own type (Hyper's `Body`, JS `ReadableStream`, etc.).

**`BackendClient`** — Makes signed outbound HTTP requests to backing object stores. The server runtime uses `reqwest`; the worker runtime uses the JS Fetch API.

**`ConfigProvider`** — Retrieves bucket, role, and credential configuration. Ships with four implementations behind feature flags:

| Provider | Feature | Use Case |
|----------|---------|----------|
| `StaticProvider` | *(always)* | TOML/JSON files, baked-in config |
| `HttpProvider` | `config-http` | Centralized config REST API |
| `DynamoDbProvider` | `config-dynamodb` | AWS-native deployments |
| `PostgresProvider` | `config-postgres` | Database-backed config |

Any provider can be wrapped with `CachedProvider` for in-memory TTL caching.

**`RequestResolver`** — Decides what to do with an incoming request. Given an HTTP method, path, query, and headers, a resolver returns a `ResolvedAction`: either forward to a backend (`Proxy`) or return a synthetic response (`Response`). This decouples URL namespace mapping, authentication, and authorization from the proxy handler itself.

`DefaultResolver<P: ConfigProvider>` implements the standard S3 proxy flow: parse the S3 operation, look up the bucket in config, authenticate via SigV4, and authorize. Custom resolvers (like the Source Cooperative resolver in `cf-workers`) can implement entirely different routing and auth schemes.

## Module Overview

```
src/
├── auth.rs          SigV4 verification, identity resolution, authorization
├── backend.rs       BackendClient trait, S3RequestSigner, outbound SigV4 signing
├── config/
│   ├── mod.rs       ConfigProvider trait definition
│   ├── cached.rs    TTL caching wrapper for any provider
│   ├── static_file.rs  TOML/JSON file provider
│   ├── http.rs      REST API provider (feature: config-http)
│   ├── dynamodb.rs  DynamoDB provider (feature: config-dynamodb)
│   └── postgres.rs  PostgreSQL provider (feature: config-postgres)
├── error.rs         ProxyError with S3-compatible error codes
├── proxy.rs         ProxyHandler — the main request handler
├── resolver.rs      RequestResolver trait, ResolvedAction, DefaultResolver
├── s3/
│   ├── request.rs   Parse incoming HTTP → S3Operation enum
│   ├── response.rs  Serialize S3 XML responses
│   └── list_rewrite.rs  Rewrite <Key>/<Prefix> values in list response XML
├── stream.rs        BodyStream trait
└── types.rs         BucketConfig, RoleConfig, StoredCredential, etc.
```

## Usage

This crate is not used directly. Runtime crates (`s3-proxy-server`, `s3-proxy-cf-workers`) depend on it and provide concrete trait implementations. If you're building a custom runtime integration, depend on this crate and implement `BodyStream`, `BackendClient`, and optionally `ConfigProvider` or `RequestResolver`.

### Standard usage with a ConfigProvider

Wrap your config provider in `DefaultResolver` for standard S3 proxy behavior (path/virtual-host parsing, SigV4 auth, scope-based authorization):

```rust
use s3_proxy_core::proxy::ProxyHandler;
use s3_proxy_core::resolver::DefaultResolver;
use s3_proxy_core::config::static_file::StaticProvider;

let backend_client = MyBackendClient::new();
let config = StaticProvider::from_file("config.toml")?;
let resolver = DefaultResolver::new(config, Some("s3.example.com".into()));

let handler = ProxyHandler::new(backend_client, resolver);

// In your HTTP handler:
let result = handler.handle_request(method, path, query, &headers, body).await;
// Convert `result` (ProxyResult<YourBodyType>) to your runtime's HTTP response.
```

### Custom resolver

For non-standard URL namespaces or external auth, implement `RequestResolver` directly:

```rust
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
        // Your custom routing, auth, and authorization logic here.
        // Return ResolvedAction::Proxy { .. } to forward to a backend,
        // or ResolvedAction::Response { .. } for synthetic responses.
        todo!()
    }
}

let handler = ProxyHandler::new(backend_client, MyResolver::new());
```

See `s3-proxy-cf-workers/src/source_resolver.rs` for a real-world example that maps a `/{account}/{repo}/{key}` namespace to dynamically-resolved S3 backends with external API authorization.

## Feature Flags

All optional — the default build has zero network dependencies:

- `config-http` — enables `HttpProvider` (adds `reqwest`)
- `config-dynamodb` — enables `DynamoDbProvider` (adds `aws-sdk-dynamodb`, `tokio`)
- `config-postgres` — enables `PostgresProvider` (adds `sqlx`)
