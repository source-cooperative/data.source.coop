# ADR-002: Runtime — Cloudflare Workers (Primary) + Regional ECS (Secondary)

**Status:** Pending
**Date:** 2026-03-14
**RFC:** RFC-001 §5

---

## Context

Source Cooperative's data proxy serves users globally, but most upstream data resides in AWS `us-west-2`. Users far from that region experience significant latency. Replicating data to additional regions is cost-prohibitive.

The proxy needs two deployment modes to serve distinct access patterns:

1. **Global, latency-sensitive reads** — the majority of traffic. Users worldwide reading public datasets. These benefit from edge deployment close to the caller.
2. **High-throughput, in-region workflows** — data pipelines (Spark, Databricks, Polars) running in `us-west-2` reading large volumes from S3 in the same region. Routing this traffic through an edge node adds unnecessary hops and latency.

The current proxy is a single ECS deployment. It handles both patterns, but serves neither optimally.

---

## Decision

### Cloudflare Workers (Primary)

The primary deployment target is Cloudflare Workers, with the proxy compiled to WebAssembly. Workers deploy to Cloudflare's edge network (330+ locations worldwide) automatically.

Key properties:

- **Global distribution without operational overhead.** Requests are served from the location closest to the caller. Onward routing to upstream storage traverses the Cloudflare backbone rather than the public internet.
- **Effectively no cold start.** Workers use V8 isolates (not containers). Cloudflare's "Shard and Conquer" consistent hashing achieves a 99.99% warm request rate.
- **No Cloudflare-imposed egress fees.** Upstream object store egress fees still apply, but Cloudflare does not charge for bandwidth out of Workers.
- **No wall-clock timeout.** CPU time limits apply per invocation, but streaming large objects is not killed mid-response due to elapsed time.
- **Predictable, low cost.** $5/mo base, $0.30/M requests, $0.02/M CPU-ms; 10M requests + 30M CPU-ms included.
- **WASM compatibility.** Rust compiles to WASM with mature toolchain support (`wasm-pack`, `worker-rs`).

### Regional ECS Deployments (Secondary)

Traditional containerised Rust services deployed into specific cloud regions on demand. Intended for high-throughput, in-region workflows where:

- Egress fees are zero or near-zero when traffic stays within the region
- Network throughput is higher and latency is lower than routing through an edge node
- The Workers path adds unnecessary hops

Regional deployments share the same Rust core as the Workers deployment. The proxy logic, auth, and authz layers are identical; only the runtime adapter differs.

### Shared STS and Credential Interoperability

Each deployment target hosts its own STS endpoint at `/.sts`. Workers and all regional ECS deployments share the same signing key material, so session credentials issued by any target are valid across all targets.

### Accepted Trade-offs

**Regional access restriction is unresolved.** Regional proxies should only be accessible to in-region consumers. Candidate mechanisms include VPC-only endpoints, IP range allowlisting, region-scoped audience claims, and regional-specific session credentials. Each has tradeoffs around operational complexity and developer experience. See RFC-001 Open Question 1.

**Two deployment targets increase operational surface.** The shared Rust core mitigates code divergence, but deployment, monitoring, and key management are duplicated.

---

## Consequences

**Benefits**

- Global users experience lower latency without data replication
- In-region workflows avoid unnecessary edge hops and egress charges
- No Cloudflare egress fees for the majority of traffic
- Effectively zero cold start for the primary deployment target
- Shared core ensures behavioural consistency across targets

**Costs / Risks**

- Two deployment targets to build, test, deploy, and monitor
- WASM compilation constrains library choices for the shared core (no `std` features that don't work in WASM)
- Regional access restriction mechanism is unresolved
- Credential interoperability across targets requires shared key material and coordinated rotation

---

## Alternatives Considered

**Single ECS deployment (current model)** — rejected. Does not address global latency without data replication. No edge presence.

**CDN in front of ECS** — considered. A traditional CDN (CloudFront, Cloudflare) can cache static responses, but the proxy's responses are not cacheable in a general-purpose CDN sense (authenticated, per-user). The proxy logic must run at the edge, not just caching.

**Workers only (no regional ECS)** — considered. Simpler operationally, but penalises high-throughput in-region workflows with unnecessary hops and potentially higher latency for large data transfers within the same cloud region.

**Lambda@Edge / CloudFront Functions** — considered. More limited runtime environment, tighter CPU and memory constraints, and AWS-specific. Workers offer a more capable and provider-neutral edge compute model.
