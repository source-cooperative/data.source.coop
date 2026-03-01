# Architecture Overview

The Source Data Proxy is an S3-compliant gateway that sits between clients and backend object stores. It provides authentication, authorization, and transparent proxying with zero-copy streaming.

## High-Level Architecture

```mermaid
flowchart LR
    Clients["S3 Clients<br>(aws-cli, boto3, SDKs)"]

    subgraph Proxy["source-coop-proxy"]
        Resolver["Request Resolver<br>(parse, auth, authorize)"]
        Handler["Proxy Handler<br>(dispatch operations)"]
        Backend["Proxy Backend<br>(runtime-specific I/O)"]
    end

    Config["Config Provider<br>(Static, HTTP, DynamoDB, Postgres)"]
    OIDC["OIDC Providers<br>(Auth0, GitHub, Keycloak)"]
    Stores["Object Stores<br>(S3, MinIO, R2, Azure, GCS)"]

    Clients <--> Resolver
    Resolver <--> Config
    Resolver <--> OIDC
    Handler <--> Backend
    Backend <--> Stores
```

## Design Principles

**Runtime-agnostic core** — The core proxy logic (`source-coop-core`) has zero runtime dependencies. No Tokio, no `worker-rs`. It compiles to both native and WASM targets.

**Two-phase handler** — The proxy handler separates request resolution from execution. `resolve_request()` determines what to do; the runtime executes it. This keeps streaming logic in runtime-specific code where it belongs.

**Presigned URLs for streaming** — GET, HEAD, PUT, and DELETE operations use presigned URLs. The runtime forwards the request directly to the backend — no buffering, no double-handling of bodies.

**Pluggable traits** — Three trait boundaries enable customization:
- `RequestResolver` — How requests are parsed, authenticated, and authorized
- `ConfigProvider` — Where configuration comes from
- `ProxyBackend` — How the runtime interacts with backends

## Key Components

| Component | Crate | Responsibility |
|-----------|-------|---------------|
| [Proxy Handler](./request-lifecycle) | `core` | Dispatch operations via presigned URLs, LIST, or multipart |
| [Request Resolver](./request-lifecycle#request-resolution) | `core` | Parse S3 requests, authenticate, authorize |
| [Config Providers](/configuration/providers/) | `core` | Load buckets, roles, credentials |
| [STS Handler](/auth/proxy-auth#oidcsts-temporary-credentials) | `sts` | OIDC token exchange, credential minting |
| [OIDC Provider](/auth/backend-auth#oidc-backend-auth) | `oidc-provider` | Self-signed JWT minting, backend credential exchange |
| [Server Runtime](./multi-runtime#server-runtime) | `server` | Tokio/Hyper HTTP server |
| [Workers Runtime](./multi-runtime#cloudflare-workers-runtime) | `cf-workers` | WASM-based Cloudflare Workers |

## Further Reading

- [Crate Layout](./crate-layout) — How the workspace is organized
- [Request Lifecycle](./request-lifecycle) — How a request flows through the proxy
- [Multi-Runtime Design](./multi-runtime) — How the same core runs on native and WASM
