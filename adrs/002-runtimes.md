# ADR-002: Runtime — Cloudflare Workers

**Status:** Proposed
**Date:** 2026-03-14
**RFC:** RFC-001 §5

---

## Context

Source Cooperative's data proxy serves users globally, but most upstream data resides in AWS `us-west-2`. Users far from that region experience significant latency. Replicating data to additional regions is cost-prohibitive.

The current proxy is a single ECS deployment. It works, but provides no edge presence for global users.

---

## Decision

### Cloudflare Workers

The deployment target is Cloudflare Workers, with the proxy compiled to WebAssembly. Workers deploy to Cloudflare's edge network (330+ locations worldwide) automatically.

Key properties:

- **Global distribution without operational overhead.** Requests are served from the location closest to the caller. Onward routing to upstream storage traverses the Cloudflare backbone rather than the public internet.
- **Effectively no cold start.** Workers use V8 isolates (not containers). Cloudflare's "Shard and Conquer" consistent hashing achieves a 99.99% warm request rate.
- **No Cloudflare-imposed egress fees.** Upstream object store egress fees still apply, but Cloudflare does not charge for bandwidth out of Workers.
- **No wall-clock timeout.** CPU time limits apply per invocation, but streaming large objects is not killed mid-response due to elapsed time.
- **Predictable, low cost.** $5/mo base, $0.30/M requests, $0.02/M CPU-ms; 10M requests + 30M CPU-ms included.
- **WASM compatibility.** Rust compiles to WASM with mature toolchain support (`wasm-pack`, `worker-rs`).

> [!NOTE]
> **Future extension: Regional ECS deployments.** For high-throughput, in-region workflows — data pipelines (Spark, Databricks, Polars) running in the same cloud region as the source data — routing through an edge node adds unnecessary hops and egress fees. Regional ECS deployments running the same Rust core could serve these workloads with lower latency and zero cross-region egress. The trait-based architecture (ADR-008) is designed to support additional runtime targets without code divergence. This can be pursued when there is demonstrated demand.

---

## Consequences

**Benefits**

- Global users experience lower latency without data replication
- No Cloudflare egress fees for the majority of traffic
- Effectively zero cold start
- Single deployment target keeps operational surface small

**Costs / Risks**

- WASM compilation constrains library choices (no `std` features that don't work in WASM)
- In-region, high-throughput workflows (e.g. bulk ETL in `us-west-2`) route through the edge rather than staying within the region — this adds latency and may incur upstream egress fees that an in-region proxy would avoid

---

## Alternatives Considered

**Single ECS deployment (current model)** — rejected. Does not address global latency without data replication. No edge presence.

**CDN in front of ECS** — considered. A traditional CDN (CloudFront, Cloudflare) can cache static responses, but the proxy's responses are not cacheable in a general-purpose CDN sense (authenticated, per-user). The proxy logic must run at the edge, not just caching.

**Workers + Regional ECS** — considered as the initial deployment. Simpler to start with Workers only and add regional ECS deployments when demand materialises. The trait-based architecture supports this without requiring upfront investment in a second deployment target.

**Lambda@Edge / CloudFront Functions** — considered. More limited runtime environment, tighter CPU and memory constraints, and AWS-specific. Workers offer a more capable and provider-neutral edge compute model.
