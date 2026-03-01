# Extending the Proxy

The proxy is designed for customization through three trait boundaries. Each controls a different aspect of the proxy's behavior.

| Trait | Controls | Default Implementation |
|-------|----------|----------------------|
| [RequestResolver](./custom-resolver) | How requests are parsed, authenticated, and authorized | `DefaultResolver` (standard S3 proxy behavior) |
| [ConfigProvider](./custom-provider) | Where configuration comes from | Static file, HTTP, DynamoDB, Postgres |
| [ProxyBackend](./custom-backend) | How the runtime interacts with backends | `ServerBackend`, `WorkerBackend` |

## When to Customize What

**Custom Resolver** — Your URL namespace doesn't map to `/{bucket}/{key}`, or you need external authorization (e.g., an API call), or you want different authentication logic.

**Custom Config Provider** — You want to store config in a backend not already supported (e.g., etcd, Redis, Consul), or you need to derive config from another source.

**Custom Backend** — You're deploying to a runtime that's neither a standard server nor Cloudflare Workers (e.g., AWS Lambda, Deno Deploy), or you need a different HTTP client.
