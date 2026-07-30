# ADR-002: Runtime — Cloudflare Workers

**Date:** 2026-03-14
**RFC:** RFC-001 §5
**Language:** ASD-STE100 Simplified Technical English

---

## Context

The data proxy of Source Cooperative serves users around the world. But most of the upstream data is in the AWS region `us-west-2`. Users far from that region get much latency. To copy the data into more regions costs too much.

The current proxy is one ECS deployment. It operates correctly, but it gives no edge presence to global users.

---

## Decision

### Cloudflare Workers

The deployment target is Cloudflare Workers, and the proxy compiles to WebAssembly. Cloudflare deploys a Worker automatically to its edge network of more than 330 locations.

The primary properties are:

- **Global distribution with no operational work.** The network serves each request from the location nearest to the caller. Traffic to the upstream storage goes across the Cloudflare backbone and not across the public internet.
- **Almost no cold start.** Workers use V8 isolates and not containers. The "Shard and Conquer" technique of Cloudflare uses consistent hashing and keeps 99.99% of the requests warm.
- **No egress fees from Cloudflare.** The upstream object store continues to charge its egress fees, but Cloudflare does not charge for bandwidth out of a Worker.
- **No wall-clock limit.** A CPU-time limit applies to each invocation. But the platform does not stop a large object in the middle of the response because too much time went by.
- **Low and predictable cost.** The base plan costs $5 each month. Then requests cost $0.30 for each million, and CPU time costs $0.02 for each million milliseconds. The base plan includes 10 million requests and 30 million CPU milliseconds.
- **WASM compatibility.** Rust compiles to WASM, and the toolchain (`wasm-pack`, `worker-rs`) is mature.

> [!NOTE]
> **Future extension: Regional ECS deployments.** Some workflows have a high throughput and operate in one region. Examples are data pipelines (Spark, Databricks, Polars) that run in the same cloud region as the source data. For these workflows, an edge node adds unwanted hops and egress fees. A regional ECS deployment with the same Rust core can serve these workloads with less latency and no cross-region egress. Multistore can support more runtime targets with no divergence of the code. We can do this work when there is a demonstrated demand.

---

## Consequences

**Benefits**

- Global users get less latency, and we do not copy the data.
- Cloudflare charges no egress fees for most of the traffic.
- There is almost no cold start.
- One deployment target keeps the operational surface small.

**Costs and Risks**

- Compilation to WASM limits the choice of libraries. We cannot use a `std` feature that does not operate in WASM.
- Some workflows have a high throughput in one region. An example is a bulk ETL job in `us-west-2`. These workflows go through the edge and do not stay in the region. This adds latency. It can also add upstream egress fees that a proxy in the same region would prevent.

---

## Alternatives Considered

**One ECS deployment (the current model)** — rejected. It does not decrease the global latency, and to decrease it we must copy the data. It has no edge presence.

**A CDN in front of ECS** — considered. A usual CDN (CloudFront or Cloudflare) caches static responses. But the responses of the proxy are authenticated and specific to one user, thus a general-purpose CDN cannot cache them. The logic of the proxy must operate at the edge, and not only the cache.

**Workers and regional ECS together** — considered as the first deployment. It is more simple to start with Workers only, and to add a regional ECS deployment when the demand occurs. The architecture of multistore supports this, thus we do not invest in a second deployment target now.

**Lambda@Edge or CloudFront Functions** — considered. Their runtime environment is more limited, their CPU and memory limits are tighter, and they are specific to AWS. Workers give a more capable edge compute model, and that model is neutral to the provider.
