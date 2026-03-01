# Deployment

The proxy can be deployed in two ways:

| | [Server Runtime](./server) | [Cloudflare Workers](./cloudflare-workers) |
|---|---|---|
| **Best for** | Container environments (ECS, K8s, Docker) | Edge deployments, low-latency global access |
| **Backends** | S3, Azure, GCS | S3 only |
| **Scaling** | Horizontal (multiple instances) | Automatic (Cloudflare edge) |
| **Config** | TOML file + env vars | Env vars (JSON) + Wrangler secrets |
| **Complexity** | Standard ops (containers, load balancers) | Managed (no infrastructure to operate) |

Both runtimes use the same core logic and support the same authentication flows. Choose based on your infrastructure preferences and backend requirements.
