# Multi-Runtime Design

The proxy runs on two runtimes — a native Tokio/Hyper server for container deployments and Cloudflare Workers for edge deployments. The same core logic compiles to both targets through careful abstraction of platform-specific concerns.

## Runtime Comparison

| | Server Runtime | CF Workers Runtime |
|---|---|---|
| **Platform** | Linux/macOS containers | Cloudflare Workers (V8) |
| **Target** | `x86_64` / `aarch64` | `wasm32-unknown-unknown` |
| **HTTP client** | reqwest | `web_sys::fetch` |
| **Streaming** | hyper `Incoming` / reqwest `bytes_stream()` | JS `ReadableStream` passthrough |
| **Object store connector** | Default (reqwest-based) | `FetchConnector` |
| **Backend support** | S3, Azure, GCS | S3 only |
| **Config loading** | TOML file | Env var (JSON or JS object) |
| **Threading** | Multi-threaded (`Send + Sync` required) | Single-threaded (`!Send` types allowed) |

## How It Works

### MaybeSend / MaybeSync

The core challenge is that Tokio requires `Send + Sync` for task spawning, while WASM runtimes are single-threaded and use `!Send` types (like `JsValue` and `ReadableStream`).

The solution is conditional trait aliases defined in `source-coop-core`:

- On native targets: `MaybeSend` resolves to `Send`, `MaybeSync` resolves to `Sync`
- On `wasm32`: `MaybeSend` and `MaybeSync` are blanket traits that every type implements

All core traits (`ProxyBackend`, `RequestResolver`, `ConfigProvider`) use `MaybeSend + MaybeSync` instead of `Send + Sync`, so they compile on both targets.

The `Signer` trait from `object_store` requires real `Send + Sync`, which works because `UnsignedUrlSigner` only holds `String` fields, and `object_store`'s built-in store types are `Send + Sync`.

### RPITIT Async Methods

Core traits use return-position `impl Trait` in trait (RPITIT) for async methods instead of `#[async_trait]`:

```rust
pub trait RequestResolver: Clone + MaybeSend + MaybeSync + 'static {
    fn resolve(
        &self,
        method: &Method,
        path: &str,
        query: Option<&str>,
        headers: &HeaderMap,
    ) -> impl Future<Output = Result<ResolvedAction, ProxyError>> + MaybeSend;
}
```

This avoids `#[async_trait]`'s `Box<dyn Future + Send>` requirement, which won't compile on WASM targets.

## Server Runtime

The server runtime (`crates/runtimes/server/`) uses Tokio and Hyper:

- **Forward actions**: reqwest sends the presigned URL request. For GET, the response body is streamed via `bytes_stream()`. For PUT, the client's hyper `Incoming` body is streamed directly to reqwest.
- **`ServerBackend`**: Creates `object_store` instances with the default HTTP connector (reqwest) and uses reqwest for `send_raw()` (multipart).

## Cloudflare Workers Runtime

The CF Workers runtime (`crates/runtimes/cf-workers/`) uses `worker-rs`, `wasm-bindgen`, and `web_sys`:

- **Forward actions**: JS `ReadableStream` bodies pass through without touching Rust. The Workers Fetch API handles streaming natively.
- **`WorkerBackend`**: Creates `object_store` instances with `FetchConnector` injected for HTTP transport.

### FetchConnector

`FetchConnector` bridges `object_store`'s `HttpConnector` trait to the Workers Fetch API. Since `worker::Fetch::send()` is `!Send`, each call is wrapped in `spawn_local` with a oneshot channel to bridge back to the `Send` context that `object_store` expects.

This is only used for LIST operations — presigned URL operations bypass `object_store` entirely.

### WASM Limitations

- **S3 only**: Azure and GCS builders are gated behind cargo features that are disabled for the Workers runtime
- **`Instant::now()` panics on WASM**: The `UnsignedUrlSigner` avoids the `InstanceCredentialProvider` → `TokenCache` → `Instant::now()` code path that panics on WASM
- **No `default-members`**: The CF Workers crate is excluded from the workspace default members. Always build with:
  ```bash
  cargo check -p source-coop-cf-workers --target wasm32-unknown-unknown
  ```
