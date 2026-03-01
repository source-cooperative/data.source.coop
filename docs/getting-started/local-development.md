# Local Development

This guide walks through setting up a full local development environment with MinIO as the backing object store.

## Docker Compose

The project includes a `docker-compose.yml` that starts MinIO and seeds it with example data:

```bash
docker compose up
```

This starts:
- **MinIO API** at `http://localhost:9000`
- **MinIO Console** at `http://localhost:9001` (credentials: `minioadmin` / `minioadmin`)
- A seed job that creates `public-data` and `private-uploads` buckets with sample files

## Configuration Files

The two runtimes use different config formats:

### Server Runtime — `config.local.toml`

The server runtime reads a TOML config file. The local development config points buckets at `http://localhost:9000` (MinIO):

```bash
cargo run -p source-coop-server -- \
  --config config.local.toml \
  --listen 0.0.0.0:8080
```

### Workers Runtime — `wrangler.toml`

The CF Workers runtime reads `PROXY_CONFIG` from the Wrangler configuration. It can be a JSON string or a JS object:

```bash
cd crates/runtimes/cf-workers && npx wrangler dev
```

The Workers dev server runs on port `8787` by default.

## Building

```bash
# Check/build default workspace members (excludes cf-workers)
cargo check
cargo build

# CF Workers must target wasm32
cargo check -p source-coop-cf-workers --target wasm32-unknown-unknown

# Run tests
cargo test
```

## Makefile

The project includes a Makefile with common tasks:

```bash
make check           # cargo check
make check-wasm      # cargo check for CF Workers (wasm32 target)
make test            # cargo test
make fmt             # check formatting
make clippy          # run linter
make run-server      # run the server runtime
make run-workers     # run the workers runtime (wrangler dev)
make ci-fast         # fmt + clippy + check-wasm
make ci              # ci-fast + test
```

## Environment Variables

For local development, these are optional but useful:

| Variable | Purpose | Example |
|----------|---------|---------|
| `SESSION_TOKEN_KEY` | AES-256-GCM key for sealed tokens | `openssl rand -base64 32` |
| `OIDC_PROVIDER_KEY` | RSA private key for OIDC backend auth | PEM file contents |
| `OIDC_PROVIDER_ISSUER` | Public URL for OIDC discovery | `http://localhost:8080` |
| `RUST_LOG` | Logging level | `source_coop=debug` |

## Verifying the Setup

Once the proxy is running, test both anonymous and authenticated access:

```bash
# Anonymous read (should return file contents)
curl http://localhost:8080/public-data/hello.txt

# Authenticated upload
AWS_ACCESS_KEY_ID=AKLOCAL0000000000001 \
AWS_SECRET_ACCESS_KEY="localdev/secret/key/00000000000000000000" \
aws s3 cp ./test.txt s3://private-uploads/test.txt \
    --endpoint-url http://localhost:8080

# List bucket contents
AWS_ACCESS_KEY_ID=AKLOCAL0000000000001 \
AWS_SECRET_ACCESS_KEY="localdev/secret/key/00000000000000000000" \
aws s3 ls s3://private-uploads/ \
    --endpoint-url http://localhost:8080

# Browse MinIO directly
open http://localhost:9001
```
