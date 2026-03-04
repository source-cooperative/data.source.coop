# data.source.coop

Runtime-agnostic core library for the S3 proxy gateway. This crate contains all business logic — S3 request parsing, SigV4 signing/verification, authorization, configuration retrieval, and the proxy handler — without depending on any async runtime.

## Why This Crate Exists Separately

The proxy needs to run on fundamentally different runtimes: Tokio/Hyper in containers and Cloudflare Workers on the edge. These runtimes have incompatible stream types, HTTP primitives, and threading models (multi-threaded vs single-threaded WASM). By keeping the core free of runtime dependencies, it compiles cleanly to both `x86_64-unknown-linux-gnu` and `wasm32-unknown-unknown`.

## Key Abstractions

The core defines four trait boundaries that runtime crates implement:

**`ProxyBackend`** — Provides three capabilities: `create_store()` returns an `ObjectStore` for LIST, `create_signer()` returns a `Signer` for presigned URL generation (GET/HEAD/PUT/DELETE), and `send_raw()` sends signed HTTP requests for multipart operations. Both runtimes delegate to `build_signer()` which uses `object_store`'s built-in signer for authenticated backends and `UnsignedUrlSigner` for anonymous backends (avoiding `Instant::now()` which panics on WASM). For `create_store()`, the server runtime uses default connectors + reqwest; the worker runtime uses a custom `FetchConnector`.

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

**`OidcBackendAuth`** — Resolves backend credentials via OIDC token exchange. Called at the top of `dispatch_operation()` before the config reaches `create_store()`/`create_signer()`. When a bucket's `backend_options` contains `auth_type=oidc`, the implementation mints a self-signed JWT and exchanges it for temporary cloud credentials, injecting them into the config. The default `NoOidcAuth` passes configs through unchanged (and errors if `auth_type=oidc` is set without a provider). The `oidc-provider` crate provides `AwsOidcBackendAuth` and `MaybeOidcAuth` as concrete implementations.

## Module Overview

```
src/
├── auth.rs          SigV4 verification, identity resolution, authorization
├── backend.rs       ProxyBackend trait, Signer/StoreBuilder, S3RequestSigner (multipart)
├── config/
│   ├── mod.rs       ConfigProvider trait definition
│   ├── cached.rs    TTL caching wrapper for any provider
│   ├── static_file.rs  TOML/JSON file provider
│   ├── http.rs      REST API provider (feature: config-http)
│   ├── dynamodb.rs  DynamoDB provider (feature: config-dynamodb)
│   └── postgres.rs  PostgreSQL provider (feature: config-postgres)
├── error.rs         ProxyError with S3-compatible error codes
├── oidc_backend.rs  OidcBackendAuth trait, NoOidcAuth default impl
├── proxy.rs         ProxyHandler — the main request handler
├── resolver.rs      RequestResolver trait, ResolvedAction, DefaultResolver
├── sealed_token.rs  AES-256-GCM encrypted session tokens (TokenKey)
├── s3/
│   ├── request.rs   Parse incoming HTTP → S3Operation enum
│   ├── response.rs  Serialize S3 XML responses
│   └── list_rewrite.rs  Rewrite <Key>/<Prefix> values in list response XML
├── response_body.rs ProxyResponseBody enum (Bytes, Empty)
└── types.rs         BucketConfig, RoleConfig, StoredCredential, etc.
```

## Usage

This crate is not used directly. Runtime crates (`source-coop-server`, `source-coop-cf-workers`) depend on it and provide concrete `ProxyBackend` implementations. If you're building a custom runtime integration, depend on this crate and implement `ProxyBackend`, and optionally `ConfigProvider` or `RequestResolver`.

### Standard usage with a ConfigProvider

Wrap your config provider in `DefaultResolver` for standard S3 proxy behavior (path/virtual-host parsing, SigV4 auth, scope-based authorization):

```rust
use source_coop_core::proxy::ProxyHandler;
use source_coop_core::resolver::DefaultResolver;
use source_coop_core::config::static_file::StaticProvider;

let backend = MyBackend::new();
let config = StaticProvider::from_file("config.toml")?;
// Optional: enable sealed session tokens for STS temporary credentials.
// When set, TemporaryCredentials are AES-256-GCM encrypted into the session
// token itself — no server-side storage needed (critical for stateless runtimes).
let token_key = None; // or Some(TokenKey::from_base64(&key_b64)?)
let resolver = DefaultResolver::new(config, Some("s3.example.com".into()), token_key);

let handler = ProxyHandler::new(backend, resolver);
// Optional: enable OIDC-based backend credential resolution.
// let handler = handler.with_oidc_auth(oidc_auth);

// In your HTTP handler:
let action = handler.resolve_request(method, path, query, &headers).await;
// Handle action: Forward (presigned URL), Response (ProxyResult), or NeedsBody (multipart)
```

### Custom resolver

For non-standard URL namespaces or external auth, implement `RequestResolver` directly:

```rust
use source_coop_core::resolver::{RequestResolver, ResolvedAction};
use source_coop_core::error::ProxyError;

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

let handler = ProxyHandler::new(backend, MyResolver::new());
```

See `crates/libs/source-coop/src/resolver.rs` for a real-world example that maps a `/{account}/{repo}/{key}` namespace to dynamically-resolved S3 backends with external API authorization.

## Sealed Session Tokens

The `sealed_token` module provides stateless temporary credential verification using AES-256-GCM. When a `TokenKey` is configured (via `SESSION_TOKEN_KEY`), the STS handler encrypts the full `TemporaryCredentials` struct into the session token itself. On subsequent requests, `resolve_identity()` decrypts the token to recover the credentials — no server-side storage or config lookup is needed.

This is critical for stateless runtimes like Cloudflare Workers where in-memory state does not persist across invocations. The `TokenKey` wraps `Arc<Aes256Gcm>` and is `Clone + Send + Sync`.

Token format: `base64url(nonce[12] || ciphertext + tag)`. Expired tokens return `Err(ExpiredCredentials)`. Tokens that fail decryption (wrong key, not a sealed token) return `Ok(None)` allowing graceful rejection.

Note: because scopes are sealed into the token at mint time, changes to a role's `allowed_scopes` in config only take effect for newly minted credentials — existing tokens retain the scopes they were issued with.

## Feature Flags

All optional — the default build has zero network dependencies:

- `config-http` — enables `HttpProvider` (adds `reqwest`)
- `config-dynamodb` — enables `DynamoDbProvider` (adds `aws-sdk-dynamodb`, `tokio`)
- `config-postgres` — enables `PostgresProvider` (adds `sqlx`)
