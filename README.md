# Source Cooperative Data Proxy

A read-only data proxy for [Source Cooperative](https://source.coop), built as a [Cloudflare Worker](https://developers.cloudflare.com/workers/) in Rust. It translates Source Cooperative URL paths into requests against cloud storage backends (S3, Azure Blob Storage, GCS) using the [multistore](https://github.com/developmentseed/multistore) S3 gateway.

The proxy supports `GET`, `HEAD`, and S3-compatible `LIST` operations with anonymous access, and resolves storage backends dynamically via the Source Cooperative API.

## Getting Started

### Prerequisites

- [Rust](https://rustup.rs/) with the `wasm32-unknown-unknown` target
- [wrangler](https://developers.cloudflare.com/workers/wrangler/) (Cloudflare Workers CLI)

```sh
rustup target add wasm32-unknown-unknown
cargo install worker-build@0.7.5
npm install -g wrangler@3
```

### Run Locally

```sh
wrangler dev
```

The proxy will be available at `http://localhost:8787`.

### Running Tests

Unit tests for routing and pagination run on native targets:

```sh
cargo test
```

Integration tests run against a live instance:

```sh
wrangler dev &
python -m pytest tests/test_integration.py
```

### Example Requests

```sh
# Version info
curl http://localhost:8787/

# List products for an account
curl "http://localhost:8787/cholmes?list-type=2&delimiter=/"

# List files in a product
curl "http://localhost:8787/cholmes/admin-boundaries?list-type=2&prefix=&max-keys=10"

# Get object metadata
curl -I http://localhost:8787/cholmes/admin-boundaries/countries.parquet

# Download with range request
curl -r 0-1023 http://localhost:8787/cholmes/admin-boundaries/countries.parquet -o chunk.bin
```

## Architecture

The proxy rewrites Source Cooperative URL paths (`/{account}/{product}/{key}`) into multistore's virtual bucket model, resolving storage backends dynamically via the Source Cooperative API.

```
Client Request: GET /{account}/{product}/{key}
  │
  ├─ [routing]   Parse path, rewrite to bucket={account}--{product}, key={key}
  ├─ [registry]  Resolve backend via Source API (product metadata + data connections)
  ├─ [cache]     Cache API responses (products: 5min, data connections: 30min, listings: 1min)
  ├─ [multistore ProxyGateway]  Generate presigned URL for the resolved storage backend
  └─ Stream response back to client
```

### Modules

| Module | Purpose |
|---|---|
| `src/lib.rs` | Fetch handler, request routing, CORS |
| `src/routing.rs` | Request classification and path rewriting |
| `src/registry.rs` | Source Cooperative API client and backend resolution |
| `src/cache.rs` | Cloudflare Cache API wrapper with per-datatype TTLs |
| `src/pagination.rs` | S3-compatible pagination for prefix listings |

### Supported Operations

| Operation | Description |
|---|---|
| `GET /` | Version info |
| `GET /{account}?list-type=2` | List products for an account |
| `GET /{account}/{product}?list-type=2` | List objects in a product (S3-compatible, with pagination) |
| `GET /{account}/{product}/{key}` | Download an object (supports range requests) |
| `HEAD /{account}/{product}/{key}` | Get object metadata |
| `OPTIONS *` | CORS preflight |

Write operations (`PUT`, `POST`, `DELETE`, `PATCH`) return `405 Method Not Allowed`.

## Configuration

Environment variables (set in `wrangler.toml` or via Cloudflare dashboard):

| Variable | Default | Description |
|---|---|---|
| `SOURCE_API_URL` | `https://source.coop` | Source Cooperative API base URL |
| `SOURCE_API_SECRET` | — | Optional API authentication token |
| `LOG_LEVEL` | `WARN` | Tracing level (`TRACE`, `DEBUG`, `INFO`, `WARN`, `ERROR`) |

## Design Documents

See [`docs/plans/`](docs/plans/) for architecture and design documents.
