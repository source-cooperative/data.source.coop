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
  ├─ [routing]   Parse path, rewrite to bucket={account}:{product}, key={key}
  ├─ [registry]  Resolve backend via Source API (product metadata + data connections)
  ├─ [cache]     Cache API responses (products: 5min, data connections: 30min, listings: 1min)
  ├─ [multistore ProxyGateway]  Generate presigned URL for the resolved storage backend
  └─ Stream response back to client
```

### Modules

| Module              | Purpose                                                        |
| ------------------- | -------------------------------------------------------------- |
| `src/lib.rs`        | Fetch handler, OIDC discovery endpoints, request routing, CORS |
| `src/routing.rs`    | Request classification and path rewriting                      |
| `src/registry.rs`   | Source Cooperative API client and backend resolution           |
| `src/cache.rs`      | Cloudflare Cache API wrapper with per-datatype TTLs            |
| `src/pagination.rs` | S3-compatible pagination for prefix listings                   |
| `src/analytics.rs`  | Cloudflare Analytics Engine request logging                    |
| `src/handlers.rs`   | Custom route handlers (index, account listing)                 |

### Supported Operations

| Operation                               | Description                                                |
| --------------------------------------- | ---------------------------------------------------------- |
| `GET /`                                 | Version info                                               |
| `GET /{account}?list-type=2`            | List products for an account                               |
| `GET /{account}/{product}?list-type=2`  | List objects in a product (S3-compatible, with pagination) |
| `GET /{account}/{product}/{key}`        | Download an object (supports range requests)               |
| `HEAD /{account}/{product}/{key}`       | Get object metadata                                        |
| `OPTIONS *`                             | CORS preflight                                             |
| `GET /.well-known/openid-configuration` | OIDC discovery document                                    |
| `GET /.well-known/jwks.json`            | JSON Web Key Set for JWT verification                      |

Write operations (`PUT`, `POST`, `DELETE`, `PATCH`) return `405 Method Not Allowed`.

## Configuration

### Environment Variables

Set in `wrangler.toml` or via the Cloudflare dashboard:

| Variable                     | Default                    | Description                                               |
| ---------------------------- | -------------------------- | --------------------------------------------------------- |
| `SOURCE_API_URL`             | `https://source.coop`      | Source Cooperative API base URL                           |
| `LOG_LEVEL`                  | `WARN`                     | Tracing level (`TRACE`, `DEBUG`, `INFO`, `WARN`, `ERROR`) |
| `OIDC_PROVIDER_ISSUER`       | `https://data.source.coop` | Issuer URL for minted JWTs and OIDC discovery             |
| `OIDC_PROVIDER_KID`          | `data-proxy-1`             | Key ID for the active signing key                         |
| `OIDC_PROVIDER_KID_PREVIOUS` | —                          | Key ID for the previous key (during rotation)             |

### Secrets

Set via `wrangler secret put`:

| Secret                       | Description                                        |
| ---------------------------- | -------------------------------------------------- |
| `OIDC_PROVIDER_KEY`          | PEM-encoded PKCS#8 RSA private key for JWT signing |
| `OIDC_PROVIDER_KEY_PREVIOUS` | Previous RSA key (optional, during rotation)       |

### Authentication Priority

The proxy authenticates to the Source Cooperative API via minting short-lived JWT (60s TTL) signed with its RSA key and sending it as a `Bearer` token

## OIDC Provider

When configured with an RSA key, the proxy acts as its own OpenID Connect identity provider. It serves standard discovery endpoints so that relying parties (such as the Source Cooperative API) can verify its JWTs.

### Endpoints

- `GET /.well-known/openid-configuration` — discovery document containing issuer and JWKS URI
- `GET /.well-known/jwks.json` — public key(s) for JWT signature verification

These endpoints are only active when `OIDC_PROVIDER_KEY` is configured.

### Setup

Generate an RSA key pair and deploy it:

```sh
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out oidc-key.pem
wrangler secret put OIDC_PROVIDER_KEY < oidc-key.pem
wrangler deploy
```

Verify the endpoints:

```sh
curl https://data.source.coop/.well-known/openid-configuration
curl https://data.source.coop/.well-known/jwks.json
```

### Key Rotation

The proxy supports zero-downtime key rotation by serving both active and previous public keys in the JWKS endpoint. Only the active key signs new tokens.

**Rotation procedure:**

1. Generate a new RSA key with a new key ID:

   ```sh
   openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out new-key.pem
   ```

2. Move the current key to the previous slot and deploy the new key:

   ```sh
   # Copy current key to previous slot
   wrangler secret put OIDC_PROVIDER_KEY_PREVIOUS  # paste current key PEM
   ```

   Update `OIDC_PROVIDER_KID_PREVIOUS` in `wrangler.toml` to the current `OIDC_PROVIDER_KID` value, then set the new key ID and deploy:

   ```sh
   wrangler secret put OIDC_PROVIDER_KEY < new-key.pem
   # Update OIDC_PROVIDER_KID in wrangler.toml to the new key ID
   wrangler deploy
   ```

3. The JWKS endpoint now serves both keys. Wait for relying parties to refresh their JWKS cache.

4. Remove the previous key:

   ```sh
   # Remove OIDC_PROVIDER_KID_PREVIOUS from wrangler.toml
   wrangler secret delete OIDC_PROVIDER_KEY_PREVIOUS
   wrangler deploy
   ```

## Design Documents

See [`docs/plans/`](docs/plans/) for architecture and design documents.
