# Cloudflare Workers

The CF Workers runtime deploys the proxy to Cloudflare's edge network. It compiles to WASM and runs in the Workers V8 environment.

## Limitations

> [!WARNING]
> - **S3 backends only** — Azure and GCS are not supported on WASM
> - **Static or API config only** — DynamoDB and Postgres providers require Tokio, which is unavailable
> - **`SESSION_TOKEN_KEY` required** — Workers are stateless, so sealed tokens are the only way to persist temporary credentials

## Configuration

### `wrangler.toml`

```toml
name = "source-coop-proxy"
main = "build/worker/shim.mjs"
compatibility_date = "2024-01-01"

[build]
command = "cargo install worker-build && worker-build --release"

[vars]
VIRTUAL_HOST_DOMAIN = "s3.example.com"

[vars.PROXY_CONFIG]
buckets = [
  { name = "public-data", backend_type = "s3", anonymous_access = true, backend_options = { endpoint = "https://s3.us-east-1.amazonaws.com", bucket_name = "my-bucket", region = "us-east-1" } }
]
roles = []
credentials = []
```

`PROXY_CONFIG` can be either:
- A JSON string (via `wrangler secret put PROXY_CONFIG`)
- A JS object (via `[vars.PROXY_CONFIG]` table in `wrangler.toml`, as shown above)

### Secrets

Set sensitive values as secrets:

```bash
wrangler secret put SESSION_TOKEN_KEY
wrangler secret put OIDC_PROVIDER_KEY
```

### Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `PROXY_CONFIG` | Yes | JSON config (buckets, roles, credentials) |
| `VIRTUAL_HOST_DOMAIN` | No | Domain for virtual-hosted requests |
| `SESSION_TOKEN_KEY` | For STS | Base64-encoded 32-byte AES-256-GCM key |
| `OIDC_PROVIDER_KEY` | For OIDC backend auth | PEM-encoded RSA private key |
| `OIDC_PROVIDER_ISSUER` | For OIDC backend auth | Public URL for JWKS discovery |

## Building

```bash
# Check
cargo check -p source-coop-cf-workers --target wasm32-unknown-unknown

# Build (via Wrangler)
cd crates/runtimes/cf-workers
npx wrangler build
```

> [!WARNING]
> Always use `--target wasm32-unknown-unknown` when checking or building the CF Workers crate. It is excluded from the workspace `default-members` because WASM types won't compile on native targets.

## Development

```bash
cd crates/runtimes/cf-workers
npx wrangler dev
```

This starts a local dev server on port `8787`.

## Deploying

```bash
cd crates/runtimes/cf-workers
npx wrangler deploy
```

## Source Cooperative Mode

When `SOURCE_API_URL` is set, the Workers runtime uses `SourceCoopResolver` instead of `DefaultResolver`. This mode:
- Resolves backends dynamically from the Source Cooperative API
- Maps URLs as `/{account_id}/{repo_id}/{key}` instead of `/{bucket}/{key}`
- Handles authorization via the Source Cooperative API permissions endpoint

This is specific to Source Cooperative deployments and is not needed for standalone proxy use.
