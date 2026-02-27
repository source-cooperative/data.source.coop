# S3 Proxy Gateway

Multi-runtime S3 gateway proxy in Rust. Each runtime have feature parity. Proxies S3-compatible API requests to backend object stores with authentication, authorization, and streaming passthrough. Uses presigned URLs via `object_store`'s `Signer` trait for GET/HEAD/PUT/DELETE (enabling zero-copy streaming), `object_store` directly for LIST, and raw signed HTTP for multipart uploads.

The intention of this codebase is to serve as a data proxy for the Source Cooperative. However, it should be structured in a way for others to use and build upon for their individual proxy needs. As such, a modular approach should be utilized to enable others to compose similar but different sytems.

## Workspace Structure

- `crates/libs/core` — Core proxy logic, traits, config, S3 request parsing
- `crates/libs/sts` — OIDC/STS token exchange (AssumeRoleWithWebIdentity, JWT validation)
- `crates/runtimes/server` — Tokio/Hyper server runtime
- `crates/runtimes/cf-workers` — Cloudflare Workers runtime (WASM)

## Build Commands

```bash
# Check/build default workspace members (excludes cf-workers)
cargo check
cargo build

# CF Workers crate MUST be checked/built with the wasm32 target:
cargo check -p source-coop-cf-workers --target wasm32-unknown-unknown

# Run tests
cargo test
```

## Key Architecture Notes

- **Two-phase handler**: `ProxyHandler::resolve_request()` returns a `HandlerAction` enum:
  - `Forward(ForwardRequest)` — presigned URL + headers for GET/HEAD/PUT/DELETE. The runtime executes the request with its native HTTP client, enabling zero-copy streaming.
  - `Response(ProxyResult)` — complete response for LIST, errors, synthetic responses.
  - `NeedsBody(PendingRequest)` — multipart operations that need the request body. The runtime materializes the body and calls `handle_with_body()`.
- **ProxyBackend trait**: `ProxyHandler<B, R, O>` is generic over `B: ProxyBackend`, `R: RequestResolver`, and `O: OidcBackendAuth` (defaults to `NoOidcAuth`). The backend trait has three methods: `create_store()` returns an `Arc<dyn ObjectStore>` for LIST, `create_signer()` returns an `Arc<dyn Signer>` for presigned URL generation, and `send_raw()` sends pre-signed HTTP requests for multipart operations. Each runtime provides its own implementation:
  - **Server**: `ServerBackend` delegates to `build_object_store()` (with default connector) and `build_signer()`, and uses reqwest for raw HTTP + Forward execution.
  - **CF Workers**: `WorkerBackend` delegates to `build_object_store()` (injecting `FetchConnector`) and `build_signer()`, and uses `web_sys::fetch` for raw HTTP + Forward execution.
- **Multi-provider support**: `BucketConfig` uses a `backend_type: String` discriminator (`"s3"`, `"az"`, `"gcs"`) and a `backend_options: HashMap<String, String>` for provider-specific config. `build_object_store()` in `crates/libs/core/src/backend.rs` dispatches on `backend_type`, iterating `backend_options` calling `with_config()` on the appropriate builder. `build_signer()` dispatches similarly: for authenticated backends it uses `object_store`'s built-in signer (WASM-safe because `StaticCredentialProvider` bypasses `Instant::now()`); for anonymous backends (no credentials) it returns `UnsignedUrlSigner` which constructs plain URLs without auth parameters (avoiding the `InstanceCredentialProvider` → `Instant::now()` panic on WASM). Azure and GCS builders are gated behind cargo features (`azure`, `gcp`) on `source-coop-core`. The server runtime enables both; the CF Workers runtime enables neither (only S3 is supported on WASM). Runtimes inject their HTTP connector via a closure over `StoreBuilder` for `build_object_store()` only — `build_signer()` needs no connector since signing is pure computation.
- **Operation dispatch** via presigned URLs and direct object_store:
  - **GET/HEAD/PUT/DELETE** → `create_signer()` generates a presigned URL, returned as `HandlerAction::Forward`. The runtime executes the URL with its native HTTP client, streaming request/response bodies directly without handler involvement.
  - **LIST** → `create_store()` + `store.list_with_delimiter()`; builds S3 ListObjectsV2 XML from `ListResult`. `IsTruncated` is always `false`.
  - **Multipart** (CreateMultipartUpload, UploadPart, CompleteMultipartUpload, AbortMultipartUpload) → `NeedsBody` then raw signed HTTP via `backend.send_raw()` + `S3RequestSigner`.
- **ProxyResponseBody**: A simple enum (`Bytes`, `Empty`) for non-streaming responses only. Streaming bodies bypass this type entirely via the `Forward` action — runtimes handle them natively.
- **RequestResolver pattern**: The resolver decides what to do with each request (parse, auth, authorize, return proxy action or synthetic response). `DefaultResolver<P: ConfigProvider>` handles standard S3 proxy behavior. Custom resolvers (e.g., `SourceCoopResolver` in cf-workers) implement product-specific namespace mapping and auth. Runtimes are thin adapters that pick a resolver and call `handler.resolve_request()`.
- **MaybeSend pattern**: Core traits use `MaybeSend`/`MaybeSync` (defined in `crates/libs/core/src/maybe_send.rs`) instead of `Send`/`Sync`. On native targets these resolve to `Send`/`Sync`; on `wasm32` they are no-op blanket traits. This allows the CF Workers runtime to use `!Send` JS interop types (`JsValue`, `ReadableStream`, etc.). The `Signer` trait from `object_store` requires real `Send + Sync`, which works because `UnsignedUrlSigner` only holds `String` fields and `object_store`'s built-in store types are `Send + Sync`.
- **FetchConnector** (CF Workers): `crates/runtimes/cf-workers/src/fetch_connector.rs` implements `object_store::client::HttpConnector` and `HttpService` using the Workers Fetch API. Since `worker::Fetch::send()` is `!Send`, each call is wrapped in `spawn_local` with a oneshot channel to bridge back to the `Send` context that `object_store` expects. Only exercised for LIST operations (presigned URL operations bypass `object_store` entirely).
- **Streaming via Forward pattern**: For GET, the runtime sends a presigned URL request and streams the response body directly to the client. For PUT, the runtime streams the client's request body directly to the presigned URL. On CF Workers, JS `ReadableStream` objects pass through without touching Rust. On the server, reqwest streams hyper `Incoming` bodies and `bytes_stream()` responses.
- **cf-workers is excluded from `default-members`** in the root `Cargo.toml` because WASM types are `!Send` and will fail to compile on native targets. Always use `--target wasm32-unknown-unknown` when working with this crate.
- **Config loading** (CF Workers): `PROXY_CONFIG` can be either a JSON string (via `wrangler secret`) or a JS object (via `[vars.PROXY_CONFIG]` table in `wrangler.toml`). Both formats are handled.
- **Sealed session tokens**: When `SESSION_TOKEN_KEY` is configured, temporary credentials minted by STS are AES-256-GCM encrypted into the session token itself (`sealed_token.rs`). On subsequent requests, `resolve_identity()` decrypts the token to recover credentials — no server-side storage or config lookup needed. This is required for stateless runtimes (CF Workers). `TokenKey` wraps `Arc<Aes256Gcm>` (Clone + Send + Sync). Token format: `base64url(nonce[12] || ciphertext + tag)`. Scopes are sealed at mint time, so config changes to `allowed_scopes` only affect newly minted credentials. The `DefaultResolver` accepts an optional `TokenKey` as its third constructor argument; the STS handler requires it when processing STS requests.
- **List response construction**: LIST responses are built directly from `object_store::ListResult` as S3 XML. When a resolver returns a `ListRewrite`, prefix stripping/adding is applied to `ObjectMeta.location` and `common_prefixes` paths before XML generation. The `list_rewrite` module in `crates/libs/core/src/s3/list_rewrite.rs` is retained for backward compatibility.
- **OIDC backend auth**: The `OidcBackendAuth` trait (`crates/libs/core/src/oidc_backend.rs`) resolves backend credentials via OIDC token exchange. When a bucket's `backend_options` contains `auth_type=oidc`, the proxy mints a self-signed JWT and exchanges it for temporary cloud credentials before the request reaches `create_store()`/`create_signer()`. The resolved credentials are injected into a cloned `BucketConfig.backend_options` so the existing builder pipeline works unmodified. `AwsOidcBackendAuth` (in `crates/libs/oidc-provider/src/backend_auth.rs`) implements this for AWS via `AssumeRoleWithWebIdentity`. `MaybeOidcAuth<H>` is an enum (`Enabled`/`Disabled`) used as the concrete `O` type by both runtimes. OIDC is configured via `OIDC_PROVIDER_KEY` (PEM secret) and `OIDC_PROVIDER_ISSUER` (URL). When configured, `/.well-known/openid-configuration` and `/.well-known/jwks.json` are served for cloud provider JWKS discovery. The `S3RequestSigner` includes `x-amz-security-token` for STS temporary credentials. Currently AWS/S3 only; Azure and GCP exchange flows are TODO.

## Known Limitations

1. **Multipart uses raw HTTP (S3 only)**: `object_store`'s `MultipartUpload` API doesn't expose upload IDs. Multipart operations use `S3RequestSigner` + raw HTTP. They are gated to `backend_type == "s3"` — non-S3 backends return an error for multipart requests and should use `PUT` (object_store handles chunking internally).
2. **LIST returns all results**: `object_store::list_with_delimiter()` fetches all pages internally. No S3-style pagination (continuation tokens, max-keys truncation). `IsTruncated` is always `false`.
3. **Azure/GCS require feature flags**: `MicrosoftAzureBuilder` and `GoogleCloudStorageBuilder` are gated behind cargo features (`azure`, `gcp`) on `source-coop-core`. The server runtime enables both; the CF Workers runtime only supports S3. Requesting an unsupported backend_type returns a `ConfigError`.

## Style

Don't support anything legacy.