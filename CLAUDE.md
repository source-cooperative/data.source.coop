# S3 Proxy Gateway

Multi-runtime S3 gateway proxy in Rust. Proxies S3-compatible API requests to backend object stores with authentication, authorization, and streaming passthrough. Uses the `object_store` crate for high-level operations (GET, HEAD, PUT, LIST) and raw signed HTTP for multipart uploads.

The intention of this codebase is to serve as a data proxy for the Source Cooperative. However, it should be structured in a way for others to use and build upon for their individual proxy needs. As such, a modular approach should be utilized to enable others to compose similar but different sytems.

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

- **ProxyBackend trait**: `ProxyHandler<B, R>` is generic over `B: ProxyBackend` and `R: RequestResolver`. The backend trait has two methods: `create_store()` returns an `Arc<dyn ObjectStore>` for high-level operations, and `send_raw()` sends pre-signed HTTP requests for multipart operations. Each runtime provides its own implementation:
  - **Server**: `ServerBackend` delegates to `build_object_store()` with identity connector and uses reqwest for raw HTTP.
  - **CF Workers**: `WorkerBackend` delegates to `build_object_store()` injecting `FetchConnector`, and uses `web_sys::fetch` for raw HTTP.
- **Multi-provider support**: `BucketConfig` uses a `backend_type: String` discriminator (`"s3"`, `"az"`, `"gcs"`) and a `backend_options: HashMap<String, String>` for provider-specific config. The `build_object_store()` function in `crates/libs/core/src/backend.rs` dispatches on `backend_type`, iterates `backend_options` calling `with_config()` on the appropriate builder (`AmazonS3Builder`, `MicrosoftAzureBuilder`, `GoogleCloudStorageBuilder`). Azure and GCS builders are gated behind cargo features (`azure`, `gcp`) on `s3-proxy-core`. The server runtime enables both; the CF Workers runtime enables neither (only S3 is supported on WASM). Runtimes inject their HTTP connector via a closure over `StoreBuilder`.
- **Operation dispatch**: The proxy handler dispatches S3 operations to different backends:
  - **GET** → `store.get_opts()` with Range/If-Match/If-None-Match header parsing; returns `ProxyResponseBody::Stream`.
  - **HEAD** → `store.head()`; returns metadata headers + empty body.
  - **PUT** → `store.put()`; request body materialized to `Bytes` by the runtime before calling the handler.
  - **LIST** → `store.list_with_delimiter()`; builds S3 ListObjectsV2 XML directly from `ListResult` (no XML rewriting needed). `IsTruncated` is always `false` (object_store fetches all pages internally).
  - **Multipart** (CreateMultipartUpload, UploadPart, CompleteMultipartUpload, AbortMultipartUpload) → raw signed HTTP via `backend.send_raw()` + `S3RequestSigner`. These use raw HTTP because `object_store`'s `MultipartUpload` API manages state internally and doesn't expose upload IDs for stateless proxying.
- **ProxyResponseBody**: A concrete enum (`Stream`, `Bytes`, `Empty`) replacing the old generic `B: BodyStream` type parameter. Runtimes convert this to their native response type at the edge. `Stream` wraps a `BoxStream<'static, Result<Bytes, object_store::Error>>`.
- **RequestResolver pattern**: The resolver decides what to do with each request (parse, auth, authorize, return proxy action or synthetic response). `DefaultResolver<P: ConfigProvider>` handles standard S3 proxy behavior. Custom resolvers (e.g., `SourceCoopResolver` in cf-workers) implement product-specific namespace mapping and auth. Runtimes are thin adapters that pick a resolver and call `handler.handle_request()`.
- **MaybeSend pattern**: Core traits use `MaybeSend`/`MaybeSync` (defined in `crates/libs/core/src/maybe_send.rs`) instead of `Send`/`Sync`. On native targets these resolve to `Send`/`Sync`; on `wasm32` they are no-op blanket traits. This allows the CF Workers runtime to use `!Send` JS interop types (`JsValue`, `ReadableStream`, etc.).
- **FetchConnector** (CF Workers): `crates/runtimes/cf-workers/src/fetch_connector.rs` implements `object_store::client::HttpConnector` and `HttpService` using the Workers Fetch API. Since `worker::Fetch::send()` is `!Send`, each call is wrapped in `spawn_local` with a oneshot channel to bridge back to the `Send` context that `object_store` expects. Response body streaming uses an mpsc channel: a `spawn_local` task reads from the Workers `ByteStream` and sends chunks through the channel, whose receiver is wrapped as an `HttpResponseBody`.
- **Streaming**: The server runtime now streams GET responses (previously buffered). The CF Workers runtime bridges `object_store`'s `BoxStream<Bytes>` to a JS `ReadableStream` via `TransformStream` — a `spawn_local` task reads Rust stream chunks and writes them to the writable side; the readable side is returned in the Response. Bytes cross the WASM boundary in chunks (lazy, not buffered).
- **cf-workers is excluded from `default-members`** in the root `Cargo.toml` because WASM types are `!Send` and will fail to compile on native targets. Always use `--target wasm32-unknown-unknown` when working with this crate.
- **Config loading** (CF Workers): `PROXY_CONFIG` can be either a JSON string (via `wrangler secret`) or a JS object (via `[vars.PROXY_CONFIG]` table in `wrangler.toml`). Both formats are handled.
- **List response construction**: LIST responses are built directly from `object_store::ListResult` as S3 XML. When a resolver returns a `ListRewrite`, prefix stripping/adding is applied to `ObjectMeta.location` and `common_prefixes` paths before XML generation. The `list_rewrite` module in `crates/libs/core/src/s3/list_rewrite.rs` is retained for backward compatibility.

## Known Limitations

1. **Multipart uses raw HTTP (S3 only)**: `object_store`'s `MultipartUpload` API doesn't expose upload IDs. Multipart operations use `S3RequestSigner` + raw HTTP. They are gated to `backend_type == "s3"` — non-S3 backends return an error for multipart requests and should use `PUT` (object_store handles chunking internally).
2. **LIST returns all results**: `object_store::list_with_delimiter()` fetches all pages internally. No S3-style pagination (continuation tokens, max-keys truncation). `IsTruncated` is always `false`.
3. **Azure/GCS require feature flags**: `MicrosoftAzureBuilder` and `GoogleCloudStorageBuilder` are gated behind cargo features (`azure`, `gcp`) on `s3-proxy-core`. The server runtime enables both; the CF Workers runtime only supports S3. Requesting an unsupported backend_type returns a `ConfigError`.
