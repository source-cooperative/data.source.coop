# Quick Start

> [!NOTE]
> This guide is for administrators setting up and running their own Source Data Proxy. If you're a user looking to access data through an existing proxy, see the [User Guide](/guide/).

The Source Data Proxy is a multi-runtime S3 gateway that proxies requests to backend object stores. This guide gets you running locally in minutes.

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (latest stable)
- [Docker](https://docs.docker.com/get-docker/) (for local development with MinIO)

## Start the Backend

Use Docker Compose to start MinIO as a local object store:

```bash
docker compose up
```

This starts:
- MinIO API on port `9000`
- MinIO Console on port `9001` (user: `minioadmin`, password: `minioadmin`)
- A seed job that creates example buckets with test data

## Run the Proxy

Choose either the native server runtime or Cloudflare Workers:

::: code-group

```bash [Server Runtime]
cargo run -p source-coop-server -- \
  --config config.local.toml \
  --listen 0.0.0.0:8080
```

```bash [Cloudflare Workers]
cd crates/runtimes/cf-workers && npx wrangler dev
```

:::

The server runtime listens on port `8080`. The Workers runtime listens on port `8787`.

## Make Your First Request

```bash
# Anonymous read from a public bucket
curl http://localhost:8080/public-data/hello.txt

# Signed upload with the local dev credential
AWS_ACCESS_KEY_ID=AKLOCAL0000000000001 \
AWS_SECRET_ACCESS_KEY="localdev/secret/key/00000000000000000000" \
aws s3 cp ./myfile.txt s3://private-uploads/myfile.txt \
    --endpoint-url http://localhost:8080
```

## Next Steps

- [Local Development](./local-development) — Detailed dev environment setup
- [Configuration](/configuration/) — Configuring buckets, roles, and credentials
- [Authentication](/auth/) — Setting up auth flows
- [Deployment](/deployment/) — Deploying to production
