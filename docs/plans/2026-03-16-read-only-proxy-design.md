# Read-Only Data Proxy Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a read-only Cloudflare Worker that proxies Source Cooperative data requests through the multistore S3 gateway, resolving backends dynamically via api.source.coop.

**Architecture:** The Worker rewrites incoming `/{account}/{product}/{key}` paths into multistore's virtual bucket model (bucket = `{account}--{product}`, key = `{key}`). A custom `SourceCoopRegistry` implements multistore's `BucketRegistry` trait, calling `api.source.coop` to resolve product metadata and storage backends. Account-level LIST and product listing are handled as special cases before the gateway.

**Tech Stack:** Rust, Cloudflare Workers (WASM), multistore crate (core + static-config patterns), worker crate, web_sys, serde, http

---

## URL Rewriting Strategy

Incoming paths are rewritten before passing to multistore's `ProxyGateway`:

| Incoming Request | Rewritten For Multistore |
|---|---|
| `GET /acct/prod/key` | `GET /acct--prod/key` (bucket=`acct--prod`, key=`key`) |
| `HEAD /acct/prod/key` | `HEAD /acct--prod/key` |
| `GET /acct?list-type=2&prefix=prod/sub/` | `GET /acct--prod?list-type=2&prefix=sub/` |
| `GET /acct?list-type=2` (no product prefix) | Handled directly — call Source API, return CommonPrefixes XML |
| `GET /` | Return version string directly |
| `PUT/POST/DELETE` anything | Return 405 directly |

The `--` separator is used because S3 bucket names cannot contain `/`. The `SourceCoopRegistry` splits on `--` to recover `(account, product)`.

---

### Task 1: Update Cargo.toml with Dependencies

**Files:**
- Modify: `Cargo.toml`

**Step 1: Update Cargo.toml**

Replace the current Cargo.toml with multistore and Worker dependencies. Use git dependencies pointing to the multistore repo since these crates aren't published to crates.io.

```toml
[package]
name = "source-data-proxy"
version = "0.1.0"
edition = "2021"
authors = ["Anthony Lukach <anthonylukach@gmail.com>"]

[lib]
crate-type = ["cdylib"]

[dependencies]
multistore = { git = "https://github.com/developmentseed/multistore", branch = "main" }
multistore-static-config = { git = "https://github.com/developmentseed/multistore", branch = "main" }

bytes = "1"
http = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"
url = "2"
futures = "0.3"

# Cloudflare Workers SDK
worker = { version = "0.7", features = ["http"] }
worker-macros = { version = "0.7", features = ["http"] }
wasm-bindgen = "0.2"
wasm-bindgen-futures = "0.4"
js-sys = "0.3"
web-sys = { version = "0.3", features = [
    "Request", "RequestInit", "RequestCache", "Response", "ResponseInit",
    "Headers", "ReadableStream", "Fetch",
] }
console_error_panic_hook = "0.1"

[dependencies.getrandom_v02]
package = "getrandom"
version = "0.2"
features = ["js"]

[dependencies.getrandom_v03]
package = "getrandom"
version = "0.3"
features = ["wasm_js"]
```

**Step 2: Verify it compiles**

Run: `npx wrangler build` or `cargo check --target wasm32-unknown-unknown`
Expected: May have dependency resolution issues to work through. Fix any version conflicts.

**Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "feat: add multistore and worker dependencies"
```

---

### Task 2: Implement Worker Infrastructure (JsBody, WorkerForwarder, WorkerBackend, Helpers)

**Files:**
- Create: `src/worker_infra.rs` — JsBody, WorkerForwarder, header conversion helpers
- Create: `src/worker_backend.rs` — WorkerBackend (ProxyBackend impl)
- Modify: `src/lib.rs` — add module declarations

These are adapted from the multistore cf-workers example (`examples/cf-workers/src/lib.rs` and `examples/cf-workers/src/client.rs`). The code is mostly copy-paste with minor adjustments.

**Step 1: Create `src/worker_infra.rs`**

Contains:
- `JsBody` struct (zero-copy body wrapper with `Option<web_sys::ReadableStream>`)
- `WorkerForwarder` implementing `Forwarder<JsBody>` — builds `web_sys::Request` from presigned URL, forwards via `worker::Fetch`, returns `ForwardResponse<web_sys::Response>`
- `collect_js_body()` — materializes JsBody into Bytes for NeedsBody path
- `convert_ws_headers()` — web_sys::Headers → http::HeaderMap
- `http_headermap_to_ws_headers()` — http::HeaderMap → web_sys::Headers
- `proxy_result_to_ws_response()` — ProxyResult → web_sys::Response
- `forward_response_to_ws()` — ForwardResponse → web_sys::Response
- `ws_error_response()` — plain text error response builder

Copy these directly from the cf-workers example `lib.rs`, keeping the same structure. Key trait bounds: `unsafe impl Send/Sync for JsBody` (Workers is single-threaded).

**Step 2: Create `src/worker_backend.rs`**

Contains `WorkerBackend` implementing `ProxyBackend`. This requires looking at the cf-workers example's `client.rs` module for the `FetchConnector` and `WorkerBackend` implementation. Adapt from the example.

**Step 3: Add module declarations to `src/lib.rs`**

```rust
mod worker_infra;
mod worker_backend;
```

**Step 4: Verify it compiles**

Run: `cargo check --target wasm32-unknown-unknown`

**Step 5: Commit**

```bash
git add src/worker_infra.rs src/worker_backend.rs src/lib.rs
git commit -m "feat: add Worker infrastructure (JsBody, Forwarder, Backend, helpers)"
```

---

### Task 3: Implement SourceCoopRegistry (BucketRegistry)

**Files:**
- Create: `src/registry.rs`

This is the core custom component. Implements `BucketRegistry` by calling `api.source.coop`.

**Step 1: Define API response types**

```rust
use serde::Deserialize;

#[derive(Deserialize)]
struct ProductResponse {
    mirrors: Vec<Mirror>,
    // other fields we don't need
}

#[derive(Deserialize)]
struct Mirror {
    connection_id: String,
    prefix: String,
    primary: bool,
}

#[derive(Deserialize)]
struct DataConnection {
    #[serde(rename = "type")]
    connection_type: String, // "s3", "az", "gcs"
    bucket: String,
    region: Option<String>,
    endpoint: Option<String>,
    access_key_id: Option<String>,
    secret_access_key: Option<String>,
    base_prefix: Option<String>,
}
```

Note: These types should be verified against the actual Source Cooperative API responses. Check `api.source.coop` API docs or the existing `data.source.coop` source code for the exact JSON shapes.

**Step 2: Implement SourceCoopRegistry**

```rust
use multistore::registry::bucket::{BucketRegistry, ResolvedBucket};
use multistore::types::{BucketConfig, ResolvedIdentity, S3Operation};
use multistore::error::ProxyError;
use multistore::api::response::BucketEntry;

#[derive(Clone)]
pub struct SourceCoopRegistry {
    api_base_url: String,
    // In-memory cache could be added later
}

impl SourceCoopRegistry {
    pub fn new(api_base_url: String) -> Self {
        Self { api_base_url }
    }

    /// Parse "account--product" bucket name into (account, product)
    fn parse_bucket_name(name: &str) -> Option<(&str, &str)> {
        name.split_once("--")
    }

    /// Fetch product metadata and build BucketConfig
    async fn resolve_product(&self, account: &str, product: &str) -> Result<BucketConfig, ProxyError> {
        // 1. GET /api/v1/products/{account}/{product}
        let product_url = format!("{}/api/v1/products/{}/{}", self.api_base_url, account, product);
        let resp: ProductResponse = self.fetch_json(&product_url).await?;

        // 2. Find primary mirror
        let mirror = resp.mirrors.iter()
            .find(|m| m.primary)
            .or_else(|| resp.mirrors.first())
            .ok_or(ProxyError::BucketNotFound(format!("{}--{}", account, product)))?;

        // 3. GET /api/v1/data-connections/{connection_id}
        let conn_url = format!("{}/api/v1/data-connections/{}", self.api_base_url, mirror.connection_id);
        let conn: DataConnection = self.fetch_json(&conn_url).await?;

        // 4. Build BucketConfig
        let mut backend_options = std::collections::HashMap::new();
        backend_options.insert("bucket_name".to_string(), conn.bucket.clone());
        if let Some(ref region) = conn.region {
            backend_options.insert("region".to_string(), region.clone());
        }
        if let Some(ref endpoint) = conn.endpoint {
            backend_options.insert("endpoint".to_string(), endpoint.clone());
        }
        if let Some(ref ak) = conn.access_key_id {
            backend_options.insert("access_key_id".to_string(), ak.clone());
        }
        if let Some(ref sk) = conn.secret_access_key {
            backend_options.insert("secret_access_key".to_string(), sk.clone());
        }
        // If no credentials, set skip_signature for anonymous backend access
        if conn.access_key_id.is_none() {
            backend_options.insert("skip_signature".to_string(), "true".to_string());
        }

        // Build prefix: data_connection.base_prefix + mirror.prefix
        let prefix = match (&conn.base_prefix, mirror.prefix.as_str()) {
            (Some(base), p) if !p.is_empty() => Some(format!("{}{}", base, p)),
            (Some(base), _) => Some(base.clone()),
            (None, p) if !p.is_empty() => Some(p.to_string()),
            _ => None,
        };

        Ok(BucketConfig {
            name: format!("{}--{}", account, product),
            backend_type: conn.connection_type,
            backend_prefix: prefix,
            anonymous_access: true, // MVP: all access is anonymous
            allowed_roles: vec![],
            backend_options,
        })
    }

    /// HTTP fetch helper using web_sys::fetch (Workers environment)
    async fn fetch_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, ProxyError> {
        let req = web_sys::Request::new_with_str(url)
            .map_err(|e| ProxyError::Internal(format!("request build failed: {:?}", e)))?;
        let worker_req: worker::Request = req.into();
        let mut resp = worker::Fetch::Request(worker_req)
            .send()
            .await
            .map_err(|e| ProxyError::Internal(format!("fetch failed: {}", e)))?;

        if resp.status_code() == 404 {
            return Err(ProxyError::BucketNotFound("not found".into()));
        }
        if resp.status_code() != 200 {
            return Err(ProxyError::Internal(format!("API returned {}", resp.status_code())));
        }

        let text = resp.text().await
            .map_err(|e| ProxyError::Internal(format!("body read failed: {}", e)))?;
        serde_json::from_str(&text)
            .map_err(|e| ProxyError::Internal(format!("JSON parse failed: {}", e)))
    }
}

impl BucketRegistry for SourceCoopRegistry {
    async fn get_bucket(
        &self,
        name: &str,
        _identity: &ResolvedIdentity,
        _operation: &S3Operation,
    ) -> Result<ResolvedBucket, ProxyError> {
        let (account, product) = Self::parse_bucket_name(name)
            .ok_or_else(|| ProxyError::BucketNotFound(name.to_string()))?;

        let config = self.resolve_product(account, product).await?;

        Ok(ResolvedBucket {
            config,
            list_rewrite: None,
        })
    }

    async fn list_buckets(
        &self,
        _identity: &ResolvedIdentity,
    ) -> Result<Vec<BucketEntry>, ProxyError> {
        // Not supported in MVP
        Ok(vec![])
    }
}
```

**Step 3: Add module declaration to `src/lib.rs`**

```rust
mod registry;
```

**Step 4: Verify it compiles**

Run: `cargo check --target wasm32-unknown-unknown`

**Step 5: Commit**

```bash
git add src/registry.rs src/lib.rs
git commit -m "feat: implement SourceCoopRegistry (BucketRegistry via api.source.coop)"
```

---

### Task 4: Implement NoopCredentialRegistry

**Files:**
- Create: `src/noop_creds.rs`

**Step 1: Implement NoopCredentialRegistry**

```rust
use multistore::registry::credential::CredentialRegistry;
use multistore::types::{StoredCredential, RoleConfig};
use multistore::error::ProxyError;

#[derive(Clone)]
pub struct NoopCredentialRegistry;

impl CredentialRegistry for NoopCredentialRegistry {
    async fn get_credential(&self, _access_key_id: &str) -> Result<Option<StoredCredential>, ProxyError> {
        Ok(None)
    }

    async fn get_role(&self, _role_id: &str) -> Result<Option<RoleConfig>, ProxyError> {
        Ok(None)
    }
}
```

**Step 2: Add module declaration to `src/lib.rs`**

**Step 3: Verify it compiles**

**Step 4: Commit**

```bash
git add src/noop_creds.rs src/lib.rs
git commit -m "feat: add NoopCredentialRegistry for anonymous-only MVP"
```

---

### Task 5: Implement URL Parsing and Rewriting

**Files:**
- Create: `src/routing.rs`

This module contains the logic to parse incoming Source Cooperative URLs and rewrite them for multistore.

**Step 1: Implement the routing module**

```rust
/// The result of parsing an incoming request URL.
pub enum ParsedRequest {
    /// Root index: GET /
    Index,
    /// Object operation: GET/HEAD /{account}/{product}/{key}
    /// Contains the rewritten path for multistore (e.g., /account--product/key)
    /// and the original query string.
    ObjectRequest {
        rewritten_path: String,
        query: Option<String>,
    },
    /// List with product prefix: GET /{account}?list-type=2&prefix=product/...
    /// Contains rewritten path and query for multistore.
    ProductList {
        rewritten_path: String,
        query: String,
    },
    /// List products for an account: GET /{account}?list-type=2 (no product in prefix)
    AccountList {
        account: String,
        query: String,
    },
    /// Write operation — reject with 405
    WriteNotAllowed,
    /// Bad request
    BadRequest(String),
}

pub fn parse_request(method: &http::Method, path: &str, query: Option<&str>) -> ParsedRequest {
    // Reject write methods
    if matches!(method, &http::Method::PUT | &http::Method::POST | &http::Method::DELETE) {
        return ParsedRequest::WriteNotAllowed;
    }

    let trimmed = path.trim_start_matches('/');

    // Root
    if trimmed.is_empty() {
        return ParsedRequest::Index;
    }

    let segments: Vec<&str> = trimmed.splitn(3, '/').collect();

    match segments.len() {
        // /{account} — either a list or bad request
        1 => {
            let account = segments[0];
            let query_str = query.unwrap_or("");

            // Check if this is an S3 list request
            if query_str.contains("list-type=2") || query_str.contains("list-type=1") {
                // Check if prefix contains a product name
                if let Some(prefix) = extract_query_param(query_str, "prefix") {
                    if let Some(slash_pos) = prefix.find('/') {
                        let product = &prefix[..slash_pos];
                        let remaining_prefix = &prefix[slash_pos + 1..];
                        // Rewrite: bucket = account--product, prefix = remaining
                        let bucket = format!("{}--{}", account, product);
                        let new_query = rewrite_prefix_in_query(query_str, remaining_prefix);
                        return ParsedRequest::ProductList {
                            rewritten_path: format!("/{}", bucket),
                            query: new_query,
                        };
                    }
                    // prefix is just a product name without slash (e.g., prefix=census)
                    // This lists objects in the product — treat as product list with empty prefix
                    let product = prefix;
                    let bucket = format!("{}--{}", account, product);
                    let new_query = rewrite_prefix_in_query(query_str, "");
                    return ParsedRequest::ProductList {
                        rewritten_path: format!("/{}", bucket),
                        query: new_query,
                    };
                }
                // No prefix — list products for this account
                return ParsedRequest::AccountList {
                    account: account.to_string(),
                    query: query_str.to_string(),
                };
            }
            ParsedRequest::BadRequest("Missing product in path".to_string())
        }
        // /{account}/{product} or /{account}/{product}/{key...}
        n if n >= 2 => {
            let account = segments[0];
            let product = segments[1];
            let key = if n == 3 { segments[2] } else { "" };
            let bucket = format!("{}--{}", account, product);

            let rewritten_path = if key.is_empty() {
                format!("/{}", bucket)
            } else {
                format!("/{}/{}", bucket, key)
            };

            // If this is a list request on /{account}/{product}?list-type=2
            if key.is_empty() && query.map_or(false, |q| q.contains("list-type=")) {
                return ParsedRequest::ProductList {
                    rewritten_path,
                    query: query.unwrap_or("").to_string(),
                };
            }

            ParsedRequest::ObjectRequest {
                rewritten_path,
                query: query.map(|s| s.to_string()),
            }
        }
        _ => ParsedRequest::BadRequest("Invalid path".to_string()),
    }
}

fn extract_query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&')
        .find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            if k == key { Some(v) } else { None }
        })
}

fn rewrite_prefix_in_query(query: &str, new_prefix: &str) -> String {
    query.split('&')
        .map(|pair| {
            if pair.starts_with("prefix=") {
                if new_prefix.is_empty() {
                    "prefix=".to_string()
                } else {
                    format!("prefix={}", new_prefix)
                }
            } else {
                pair.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}
```

**Step 2: Add module declaration to `src/lib.rs`**

**Step 3: Verify it compiles**

**Step 4: Commit**

```bash
git add src/routing.rs src/lib.rs
git commit -m "feat: implement URL parsing and rewriting for Source Coop paths"
```

---

### Task 6: Implement the Worker Fetch Handler

**Files:**
- Modify: `src/lib.rs`

Wire everything together: parse URL, handle special cases, delegate to ProxyGateway.

**Step 1: Implement the fetch handler**

```rust
use worker::*;

mod worker_infra;
mod worker_backend;
mod registry;
mod noop_creds;
mod routing;

use worker_infra::*;
use worker_backend::WorkerBackend;
use registry::SourceCoopRegistry;
use noop_creds::NoopCredentialRegistry;
use routing::{parse_request, ParsedRequest};

use multistore::proxy::{GatewayResponse, ProxyGateway};
use multistore::route_handler::RequestInfo;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[event(fetch)]
async fn fetch(req: web_sys::Request, env: Env, _ctx: Context) -> Result<web_sys::Response> {
    console_error_panic_hook::set_once();

    let api_base_url = env.var("SOURCE_API_URL")
        .map(|v| v.to_string())
        .unwrap_or_else(|_| "https://api.source.coop".to_string());

    let registry = SourceCoopRegistry::new(api_base_url.clone());
    let creds = NoopCredentialRegistry;

    let gateway = ProxyGateway::new(
        WorkerBackend,
        registry,
        creds,
        WorkerForwarder,
        None, // no virtual host domain
    );

    // Extract body stream BEFORE any wrapping
    let js_body = JsBody(req.body());

    // Parse request metadata
    let method: http::Method = req.method().parse().unwrap_or(http::Method::GET);
    let url_str = req.url();
    let uri: http::Uri = url_str.parse().unwrap();
    let path = uri.path().to_string();
    let query = uri.query().map(|q| q.to_string());
    let headers = convert_ws_headers(&req.headers());

    // Add CORS headers to all responses
    let add_cors = |resp: web_sys::Response| -> web_sys::Response {
        if let Ok(h) = resp.headers() {
            let _ = h.set("access-control-allow-origin", "*");
            let _ = h.set("access-control-allow-methods", "GET, HEAD, OPTIONS");
            let _ = h.set("access-control-allow-headers", "*");
            let _ = h.set("access-control-expose-headers", "*");
        }
        resp
    };

    // Handle OPTIONS preflight
    if method == http::Method::OPTIONS {
        return Ok(add_cors(ws_error_response(204, "")));
    }

    match parse_request(&method, &path, query.as_deref()) {
        ParsedRequest::Index => {
            Ok(add_cors(ws_error_response(200, &format!("Source Cooperative Data Proxy v{}", VERSION))))
        }
        ParsedRequest::WriteNotAllowed => {
            Ok(add_cors(ws_error_response(405, "Method Not Allowed")))
        }
        ParsedRequest::BadRequest(msg) => {
            Ok(add_cors(ws_error_response(400, &msg)))
        }
        ParsedRequest::AccountList { account, query: _ } => {
            // TODO: Call api.source.coop to list products, return CommonPrefixes XML
            // For now, return empty list
            let xml = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>{}</Name><Prefix></Prefix><IsTruncated>false</IsTruncated></ListBucketResult>"#,
                account
            );
            let resp = ws_xml_response(200, &xml);
            Ok(add_cors(resp))
        }
        ParsedRequest::ObjectRequest { rewritten_path, query } |
        ParsedRequest::ProductList { rewritten_path, query: query_str } => {
            let q = match &parsed {
                // Handle both variants
            };
            let req_info = RequestInfo::new(
                &method,
                &rewritten_path,
                query_ref,
                &headers,
                None,
            );
            let result = gateway.handle_request(&req_info, js_body, collect_js_body).await;
            Ok(add_cors(match result {
                GatewayResponse::Response(r) => proxy_result_to_ws_response(r),
                GatewayResponse::Forward(r) => forward_response_to_ws(r),
            }))
        }
    }
}
```

Note: The match arms for `ObjectRequest` and `ProductList` need careful handling — both delegate to multistore but with different query string sources. The actual implementation should properly extract the query reference for each variant.

Also add `ws_xml_response` helper to `worker_infra.rs`:
```rust
pub fn ws_xml_response(status: u16, xml: &str) -> web_sys::Response {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    let headers = web_sys::Headers::new().unwrap();
    let _ = headers.set("content-type", "application/xml");
    init.set_headers(&headers.into());
    web_sys::Response::new_with_opt_str_and_init(Some(xml), &init)
        .unwrap_or_else(|_| ws_error_response(500, "Internal Server Error"))
}
```

**Step 2: Update wrangler.toml**

Add `SOURCE_API_URL` variable:

```toml
name = "source-data-proxy"
main = "build/worker/shim.mjs"
compatibility_date = "2026-03-17"

[build]
command = "cargo install -q worker-build@0.7 && worker-build --release"

[vars]
SOURCE_API_URL = "https://api.source.coop"
```

**Step 3: Verify it compiles**

Run: `cargo check --target wasm32-unknown-unknown`

**Step 4: Commit**

```bash
git add src/lib.rs wrangler.toml
git commit -m "feat: wire up Worker fetch handler with URL routing and ProxyGateway"
```

---

### Task 7: Implement Account-Level Product Listing

**Files:**
- Modify: `src/registry.rs` — add `list_products` method
- Modify: `src/lib.rs` — flesh out the `AccountList` handler

**Step 1: Add product listing to SourceCoopRegistry**

```rust
impl SourceCoopRegistry {
    pub async fn list_products(&self, account: &str) -> Result<Vec<String>, ProxyError> {
        let url = format!("{}/api/v1/products/{}", self.api_base_url, account);
        // The API response format needs to be verified — adjust types accordingly
        let products: Vec<ProductSummary> = self.fetch_json(&url).await?;
        Ok(products.into_iter().map(|p| p.name).collect())
    }
}
```

**Step 2: Generate S3 CommonPrefixes XML in the AccountList handler**

```rust
ParsedRequest::AccountList { account, query: _ } => {
    let products = registry.list_products(&account).await
        .unwrap_or_default();
    let prefixes_xml: String = products.iter()
        .map(|p| format!("<CommonPrefixes><Prefix>{}/</Prefix></CommonPrefixes>", p))
        .collect();
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>{}</Name><Prefix></Prefix><Delimiter>/</Delimiter><IsTruncated>false</IsTruncated>{}</ListBucketResult>"#,
        account, prefixes_xml
    );
    Ok(add_cors(ws_xml_response(200, &xml)))
}
```

**Step 3: Verify it compiles**

**Step 4: Commit**

```bash
git add src/registry.rs src/lib.rs
git commit -m "feat: implement account-level product listing as S3 CommonPrefixes"
```

---

### Task 8: End-to-End Testing with wrangler dev

**Files:**
- Modify: `wrangler.toml` (if needed for local dev)

**Step 1: Start the worker locally**

Run: `npx wrangler dev`

**Step 2: Test the index endpoint**

Run: `curl http://localhost:8787/`
Expected: `Source Cooperative Data Proxy v0.1.0`

**Step 3: Test object retrieval**

Test against a known public product on Source Cooperative:

Run: `curl -I http://localhost:8787/{known-account}/{known-product}/{known-key}`
Expected: 200 with Content-Type, Content-Length, ETag headers

Run: `curl -r 0-1023 http://localhost:8787/{known-account}/{known-product}/{known-key} | wc -c`
Expected: 1024 (range request works)

**Step 4: Test listing**

Run: `curl "http://localhost:8787/{known-account}?list-type=2&prefix={known-product}/&max-keys=5"`
Expected: S3 ListBucketResult XML with up to 5 entries

Run: `curl "http://localhost:8787/{known-account}?list-type=2&delimiter=/"`
Expected: S3 ListBucketResult XML with CommonPrefixes for products

**Step 5: Test error cases**

Run: `curl -X PUT http://localhost:8787/test/test/test`
Expected: 405 Method Not Allowed

Run: `curl http://localhost:8787/nonexistent-account/nonexistent-product/file.txt`
Expected: 404

**Step 6: Test CORS**

Run: `curl -X OPTIONS -H "Origin: https://example.com" http://localhost:8787/`
Expected: 204 with CORS headers

**Step 7: Commit any fixes**

```bash
git add -A
git commit -m "fix: resolve issues found during end-to-end testing"
```

---

## Implementation Notes

- **API response shapes**: The `ProductResponse`, `DataConnection`, and product listing types in `src/registry.rs` are best-guesses based on the existing `data.source.coop` source. They MUST be verified against the actual `api.source.coop` responses during implementation. Use `curl https://api.source.coop/api/v1/products/{account}/{product}` to check.
- **Caching**: The MVP does not include caching. Each request hits `api.source.coop`. Caching (60s TTL) should be added as a fast-follow.
- **Multistore modifications**: If multistore's S3 request parser or other internals need changes to support this use case, make those changes in the multistore repo and update the git dependency.
- **The `--` separator**: If account names or product names ever contain `--`, this will break. Consider using a more unique separator or encoding scheme if this becomes an issue.
