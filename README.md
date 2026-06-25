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

| Variable                     | Default                     | Description                                                                                                                        |
| ---------------------------- | --------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| `SOURCE_API_URL`             | `https://source.coop`       | Source Cooperative API base URL                                                                                                    |
| `LOG_LEVEL`                  | `WARN`                      | Tracing level (`TRACE`, `DEBUG`, `INFO`, `WARN`, `ERROR`)                                                                          |
| `AUTH_ISSUER`                | `https://auth.source.coop`  | OIDC issuer trusted for `/.sts` token exchange                                                                                     |
| `AUTH_AUDIENCE`              | —                           | Comma-separated OAuth client ID(s) that `/.sts` subject tokens must be issued to (`aud` claim); a token is accepted if it matches any. Unset = `/.sts` token exchange is disabled (returns 501) |
| `OIDC_PROVIDER_ISSUER`       | `https://data.source.coop`  | Issuer URL for minted JWTs and OIDC discovery                                                                                      |
| `OIDC_PROVIDER_KID`          | `data-proxy-1`              | Key ID for the active signing key                                                                                                  |
| `OIDC_PROVIDER_KID_PREVIOUS` | —                           | Key ID for the previous key (during rotation)                                                                                      |

### Secrets

**GitHub environment secrets are the source of truth.** The deploy workflow
(`.github/workflows/deploy.yml`) re-uploads them via `wrangler secret bulk` on
every deploy, so a value set directly with `wrangler secret put` is silently
overwritten by the next deploy. Set them per GitHub environment (`preview`,
`staging`, `production`) with `gh secret set <NAME> --env <environment>` or in
the repository settings UI.

| Secret                       | Description                                              |
| ---------------------------- | -------------------------------------------------------- |
| `OIDC_PROVIDER_KEY`          | PEM-encoded PKCS#8 RSA private key for JWT signing       |
| `OIDC_PROVIDER_KEY_PREVIOUS` | Previous RSA key (optional, during rotation)             |
| `SESSION_TOKEN_KEY`          | Base64-encoded 32-byte AES key sealing STS credentials   |

### Authentication Priority

The proxy authenticates to the Source Cooperative API via minting short-lived JWT (60s TTL) signed with its RSA key and sending it as a `Bearer` token

## OIDC Provider

When configured with an RSA key, the proxy acts as its own OpenID Connect identity provider. It serves standard discovery endpoints so that relying parties (such as the Source Cooperative API) can verify its JWTs.

### Endpoints

- `GET /.well-known/openid-configuration` — discovery document containing issuer and JWKS URI
- `GET /.well-known/jwks.json` — public key(s) for JWT signature verification

These endpoints are only active when `OIDC_PROVIDER_KEY` is configured.

### Setup

Generate an RSA key pair, store it as a GitHub environment secret, and deploy:

```sh
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out oidc-key.pem
gh secret set OIDC_PROVIDER_KEY --env production < oidc-key.pem
```

Verify the endpoints:

```sh
curl https://data.source.coop/.well-known/openid-configuration
curl https://data.source.coop/.well-known/jwks.json
```

### Key Rotation

The proxy supports zero-downtime key rotation by serving both active and previous public keys in the JWKS endpoint. Only the active key signs new tokens.

Because CI re-uploads secrets from GitHub on every deploy, rotation happens
through GitHub environment secrets and `wrangler.toml`, not `wrangler secret put`
(which the next deploy would silently revert).

**Rotation procedure** (repeat per environment — `preview`, `staging`, `production`):

1. Generate a new RSA key with a new key ID:

   ```sh
   openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out new-key.pem
   ```

2. Move the current key to the previous slot and stage the new key:

   ```sh
   # Copy the CURRENT key PEM into the previous slot
   gh secret set OIDC_PROVIDER_KEY_PREVIOUS --env production < current-key.pem
   gh secret set OIDC_PROVIDER_KEY --env production < new-key.pem
   ```

   In `wrangler.toml`, set `OIDC_PROVIDER_KID_PREVIOUS` to the current
   `OIDC_PROVIDER_KID` value and set `OIDC_PROVIDER_KID` to the new key ID.
   Merge and let CI deploy.

3. The JWKS endpoint now serves both keys. Wait for relying parties to refresh their JWKS cache.

4. Remove the previous key:

   ```sh
   gh secret delete OIDC_PROVIDER_KEY_PREVIOUS --env production
   # The worker-side copy isn't removed by CI, so delete it once manually:
   wrangler secret delete OIDC_PROVIDER_KEY_PREVIOUS
   ```

   Remove `OIDC_PROVIDER_KID_PREVIOUS` from `wrangler.toml`, merge, and let CI deploy.

### Rotating `SESSION_TOKEN_KEY`

`SESSION_TOKEN_KEY` seals the temporary credentials issued by `/.sts`. There is
no dual-key window: rotating it immediately invalidates every outstanding sealed
credential (e.g. the frontend's `sc_proxy_creds` cookies), and affected clients
must redo the token exchange. To rotate:

```sh
openssl rand -base64 32 | gh secret set SESSION_TOKEN_KEY --env production
```

The next deploy picks it up.

## Design Documents

See [`docs/plans/`](docs/plans/) for architecture and design documents.
