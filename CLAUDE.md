# S3 Proxy Gateway

Multi-runtime S3 gateway proxy in Rust. Proxies S3-compatible API requests to backend object stores with authentication, authorization, and streaming passthrough.

## Workspace Structure

- `crates/libs/core` — Core proxy logic, traits, config, S3 request parsing
- `crates/libs/auth` — Authentication (SigV4 verification, JWT)
- `crates/runtimes/server` — Tokio/Hyper server runtime
- `crates/runtimes/cf-workers` — Cloudflare Workers runtime (WASM)

## Build Commands

```bash
# Check/build default workspace members (excludes cf-workers)
cargo check
cargo build

# CF Workers crate MUST be checked/built with the wasm32 target:
cargo check -p s3-proxy-cf-workers --target wasm32-unknown-unknown

# Run tests
cargo test
```

## Key Architecture Notes

- **RequestResolver pattern**: `ProxyHandler<C, R>` is generic over a `RequestResolver` trait. The resolver decides what to do with each request (parse, auth, authorize, return proxy action or synthetic response). `DefaultResolver<P: ConfigProvider>` handles standard S3 proxy behavior. Custom resolvers (e.g., `SourceCoopResolver` in cf-workers) implement product-specific namespace mapping and auth. Runtimes are thin adapters that pick a resolver and call `handler.handle_request()`.
- **MaybeSend pattern**: Core traits use `MaybeSend`/`MaybeSync` (defined in `crates/libs/core/src/maybe_send.rs`) instead of `Send`/`Sync`. On native targets these resolve to `Send`/`Sync`; on `wasm32` they are no-op blanket traits. This allows the CF Workers runtime to use `!Send` JS interop types (`JsValue`, `ReadableStream`, etc.).
- **cf-workers is excluded from `default-members`** in the root `Cargo.toml` because WASM types are `!Send` and will fail to compile on native targets. Always use `--target wasm32-unknown-unknown` when working with this crate.
- **Streaming passthrough**: The CF Workers runtime passes `ReadableStream` bodies through opaquely — bytes never enter Rust memory for GET/PUT requests. The `WorkerBody` enum wraps `Bytes`, `ReadableStream`, or `Empty`.
- **Config loading** (CF Workers): `PROXY_CONFIG` can be either a JSON string (via `wrangler secret`) or a JS object (via `[vars.PROXY_CONFIG]` table in `wrangler.toml`). Both formats are handled.
- **List response rewriting**: When a resolver returns `ResolvedAction::Proxy` with a `ListRewrite`, the proxy handler buffers the (small) list XML response and rewrites `<Key>` and `<Prefix>` element values — stripping a backend prefix and optionally prepending a new one. This is handled in `crates/libs/core/src/s3/list_rewrite.rs`.
