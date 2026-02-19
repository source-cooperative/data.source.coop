# s3-proxy-cf-workers

Cloudflare Workers runtime for the S3 proxy gateway. Deploys the proxy to the edge using Cloudflare's global network, with JS `ReadableStream` passthrough — response bytes from the backing store never touch WASM memory.

## How It Works

```
Client request
    -> Worker fetch handler (lib.rs)
    -> Convert worker::Request -> http types
    -> Pick resolver:
       - SOURCE_API_URL set? -> SourceCoopResolver (dynamic Source Cooperative backends)
       - Otherwise           -> DefaultResolver (static PROXY_CONFIG)
    -> ProxyHandler::handle_request() (from s3-proxy-core)
    -> WorkerBackendClient sends fetch() to backend
    -> Response returned to client (ReadableStream passthrough)
```

The `WorkerBackendClient` uses the Workers Fetch API for outbound requests. For GET responses, the JS `ReadableStream` from the backend is passed directly to the outbound `Response` — bytes never cross the WASM boundary.

## Module Overview

```
src/
├── lib.rs              Worker entry point, request/response conversion (thin adapter)
├── body.rs             WorkerBody implementing BodyStream (Bytes | ReadableStream | Empty)
├── client.rs           WorkerBackendClient implementing BackendClient via Fetch API
├── source_api.rs       HTTP client for the Source Cooperative API
└── source_resolver.rs  SourceCoopResolver implementing RequestResolver
```

## Operating Modes

### Static Config Mode (default)

Reads bucket configuration from the `PROXY_CONFIG` environment variable. Uses `DefaultResolver` which handles standard S3 path/virtual-host parsing, SigV4 authentication, and scope-based authorization.

```toml
# wrangler.toml
[vars]
PROXY_CONFIG = '{"buckets":[...],"roles":[...],"credentials":[...]}'
VIRTUAL_HOST_DOMAIN = "s3.example.com"  # optional, for virtual-hosted style
```

### Source Cooperative Mode

When `SOURCE_API_URL` is set, the worker uses `SourceCoopResolver` which resolves backends dynamically from the Source Cooperative API. This resolver implements a custom URL namespace:

- `GET /` — synthetic empty ListBuckets
- `GET /{account_id}` — lists repositories via Source API, returns synthetic ListObjectsV2 with CommonPrefixes
- `GET /{account_id}?prefix=repo_id/subdir/` — proxies to the repo's backend with prefix rewriting
- `GET|PUT|... /{account_id}/{repo_id}/{key}` — proxies to the repo's S3 backend

Authentication is handled by the Source API permissions endpoint rather than the core auth module.

```toml
# wrangler.toml
[vars]
SOURCE_API_URL = "https://api.source.coop"

# Set via wrangler secret:
# wrangler secret put SOURCE_API_KEY
```

### Implementing a Custom Resolver

To add a new operating mode, implement `RequestResolver` in a new module:

```rust
use s3_proxy_core::resolver::{RequestResolver, ResolvedAction, ListRewrite};
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
        // Parse the URL, authenticate, resolve a BucketConfig,
        // and return ResolvedAction::Proxy or ResolvedAction::Response.
        todo!()
    }
}
```

Then add a branch in `lib.rs`:

```rust
if let Ok(my_config) = env.var("MY_MODE") {
    let resolver = MyResolver::new(/* ... */);
    let handler = ProxyHandler::new(client::WorkerBackendClient, resolver);
    let result = handler.handle_request(method, &path, query.as_deref(), &headers, body).await;
    return build_worker_response(result);
}
```

## Local Development

Run MinIO via Docker Compose from the repo root, then start the worker with Wrangler:

```bash
# Terminal 1: start MinIO (from repo root)
docker compose up

# Terminal 2: start the worker dev server
cd crates/runtimes/cf-workers
npx wrangler dev
```

Wrangler starts a local server (default `:8787`). The `wrangler.toml` includes a `PROXY_CONFIG` var pointing at `localhost:9000` (MinIO).

```bash
# Test it
curl http://localhost:8787/public-data/hello.txt
```

Note: `wrangler dev` runs the WASM module in a local Workerd runtime. Outbound `fetch()` calls from the worker to `localhost:9000` work because Wrangler's dev server runs on the host network.

## Deployment

```bash
cd crates/runtimes/cf-workers

# Build and deploy to Cloudflare
npx wrangler deploy
```

For production, update the `PROXY_CONFIG` var in `wrangler.toml` (or set it via the Cloudflare dashboard / `wrangler secret`) to point at your real backend endpoints.

## Why a Separate Crate

Cloudflare Workers compile to `wasm32-unknown-unknown` and link against `worker-rs`, `wasm-bindgen`, and `web-sys`. These dependencies are incompatible with native targets. Keeping them isolated means `cargo build` for the server crate doesn't pull in WASM tooling, and `wrangler build` for this crate doesn't pull in Tokio.

This crate must always be built with `--target wasm32-unknown-unknown`:

```bash
cargo check -p s3-proxy-cf-workers --target wasm32-unknown-unknown
```
