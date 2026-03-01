---
layout: home

hero:
  name: Source Data Proxy
  text: Multi-runtime S3 gateway proxy
  tagline: A Radiant Earth project. Stream S3-compatible requests to backend object stores with authentication, authorization, and zero-copy passthrough.
  actions:
    - theme: brand
      text: User Guide
      link: /guide/
    - theme: alt
      text: Administration
      link: /getting-started/
    - theme: alt
      text: View on GitHub
      link: https://github.com/source-cooperative/data.source.coop

features:
  - title: Multi-Runtime
    details: Deploy as a native Tokio/Hyper server in containers, or as a Cloudflare Worker at the edge. Same core logic, different runtimes.
  - title: Multi-Provider
    details: Proxy to AWS S3, MinIO, Cloudflare R2, Azure Blob Storage, or Google Cloud Storage through a unified S3-compatible API.
  - title: OIDC/STS Authentication
    details: Exchange OIDC tokens from any identity provider (GitHub Actions, Auth0, Keycloak, Cognito, Ory) for scoped temporary credentials via AssumeRoleWithWebIdentity.
  - title: Zero-Copy Streaming
    details: Presigned URLs enable direct streaming between clients and backends. No buffering, no double-handling of request or response bodies.
  - title: Modular Architecture
    details: Compose your own proxy with pluggable traits for backends, config providers, and request resolvers. Use the defaults or bring your own.
  - title: OIDC Backend Auth
    details: The proxy acts as its own OIDC identity provider to authenticate with cloud backends — no long-lived credentials needed.
---

## How It Works

```mermaid
flowchart LR
    Clients["S3 Clients\n(aws-cli, boto3, SDKs)"]

    subgraph Proxy["source-coop-proxy"]
        Auth["Auth\n(STS, OIDC, SigV4)"]
        Core["Core\n(Proxy Handler)"]
        Config["Config\n(Static, HTTP, DynamoDB, Postgres)"]
    end

    Backend["Backend Stores\n(AWS S3, MinIO, R2, Azure, GCS)"]

    Clients <--> Proxy
    Proxy <--> Backend
```

The proxy sits between S3-compatible clients and backend object stores. It authenticates incoming requests, authorizes them against configured scopes, and forwards them to the appropriate backend using presigned URLs for zero-copy streaming.
