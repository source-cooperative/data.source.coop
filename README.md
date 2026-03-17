# Source Cooperative Data Proxy

A read-only data proxy for [Source Cooperative](https://source.coop), built as a Cloudflare Worker using the [multistore](https://github.com/developmentseed/multistore) S3 gateway.

Proxies requests to cloud storage backends (S3, Azure, GCS) based on product metadata from the Source Cooperative API. Supports GET, HEAD, and S3-compatible LIST operations with anonymous access.

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
Client: GET /{account}/{product}/{key}
  → Parse path, rewrite as S3 request (bucket={account}--{product}, key={key})
  → Resolve backend via source.coop API (product metadata + data connections)
  → Generate presigned URL for backend storage
  → Stream response back to client (zero-copy)
```

See [`docs/plans/`](docs/plans/) for the full design document.

## Configuration

Environment variables (set in `wrangler.toml` or via Cloudflare dashboard):

| Variable | Default | Description |
|---|---|---|
| `SOURCE_API_URL` | `https://source.coop` | Source Cooperative API base URL |
