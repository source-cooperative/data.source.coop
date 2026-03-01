# Server Runtime

The server runtime uses Tokio and Hyper to run as a native HTTP server. It supports all backend providers (S3, Azure, GCS) and all config providers.

## Building

```bash
# Default build (S3 + Azure + GCS backends)
cargo build --release -p source-coop-server

# With additional config providers
cargo build --release -p source-coop-server \
  --features source-coop-core/config-dynamodb \
  --features source-coop-core/config-postgres
```

The binary is located at `target/release/source-coop-proxy`.

## Running

```bash
./target/release/source-coop-proxy \
  --config config.toml \
  --listen 0.0.0.0:8080
```

### CLI Arguments

| Flag | Default | Description |
|------|---------|-------------|
| `--config` | (required) | Path to the TOML config file |
| `--listen` | `0.0.0.0:8080` | Address and port to listen on |
| `--domain` | (none) | Domain for virtual-hosted-style requests (e.g., `s3.example.com`) |
| `--sts-config` | (none) | Optional separate TOML file for STS roles/credentials |

### Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `SESSION_TOKEN_KEY` | For STS | Base64-encoded 32-byte AES-256-GCM key for sealed tokens |
| `OIDC_PROVIDER_KEY` | For OIDC backend auth | PEM-encoded RSA private key |
| `OIDC_PROVIDER_ISSUER` | For OIDC backend auth | Publicly reachable URL for JWKS discovery |
| `RUST_LOG` | No | Logging level (default: `source_coop=info`) |

Generate a session token key:

```bash
export SESSION_TOKEN_KEY=$(openssl rand -base64 32)
```

## Docker

```bash
# Build
docker build -t source-coop-proxy .

# Run
docker run \
  -v ./config.toml:/etc/source-coop-proxy/config.toml \
  -p 8080:8080 \
  -e SESSION_TOKEN_KEY="$SESSION_TOKEN_KEY" \
  source-coop-proxy
```

## Config Caching

The server binary wraps the config provider with `CachedProvider` (60-second TTL). Config changes from network-backed providers (HTTP, DynamoDB, Postgres) are picked up within 60 seconds without restarting the proxy.

For static file configs, changes require a restart.

## Virtual-Hosted Style

To support virtual-hosted-style requests (`bucket.s3.example.com/key`), use the `--domain` flag:

```bash
./source-coop-proxy --config config.toml --domain s3.example.com
```

Configure DNS so that `*.s3.example.com` resolves to the proxy. The proxy extracts the bucket name from the `Host` header.

Without `--domain`, only path-style requests are supported (`/bucket/key`).
