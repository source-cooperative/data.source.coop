# ADR-009: Configuration Layer — Policy Store Implementation and Caching Strategy

**Status:** Pending
**Date:** 2026-03-14
**RFC:** RFC-001 §12
**Depends on:** ADR-005

---

## Context

The authorization model (ADR-005) requires per-request lookups against a policy store for every non-public authenticated request. This is not optional — it is what enables dynamic permissions to reflect changes (new organisations, new dataset grants) in near real-time. Unlike a design that encodes permissions in the session token, this design explicitly trades token self-sufficiency for permission freshness.

This constraint means the policy store is on the **hot path** of every authenticated request to a non-public resource. The question is not *whether* the proxy needs a policy store at request time, but *how* that access is implemented with acceptable latency and availability.

---

## Decision

### Access Patterns

The proxy's configuration access has two distinct profiles:

**High-frequency, latency-sensitive (per-request)**
- Bucket public flag lookup — `bucket_id -> {public, backend_config}`
- User grant lookup — `(user_id, bucket_id) -> {granted, prefix_restrictions}`
- User bucket list — `user_id -> [bucket_ids]`

These must complete in single-digit milliseconds. In-process caching absorbs most of the load; the underlying lookup must be fast for cache misses.

**Low-frequency, management (background)**
- Issuer JWKS refresh
- Role definition updates
- Provider credential rotation

These are not on the request hot path and can tolerate higher latency.

### Implementation Options (Unresolved)

The implementation choice between the following options is unresolved and is the primary focus of RFC review:

**Option A — REST API intermediary with aggressive caching**

The proxy calls the existing Source Cooperative API for configuration lookups, wrapped in multi-layer caching: in-process (per-isolate or per-container) with short TTL, backed by Workers KV or ElastiCache as a shared distributed cache tier.

*Advantages:* The Next.js application remains the schema owner; the proxy does not need direct database credentials; the API can enforce schema constraints.
*Risks:* The REST API is an availability dependency on the hot path. A cache miss on a cold Workers isolate hitting a degraded API directly impacts request latency.

**Option B — Direct DynamoDB access**

The proxy connects directly to DynamoDB tables for configuration lookups. In-process caching still applies.

*Advantages:* DynamoDB read latency (single-digit milliseconds) is appropriate for the hot path; eliminates availability coupling to the Next.js application.
*Risks:* Two systems (proxy and Next.js) accessing the same DynamoDB tables creates a schema governance problem. DynamoDB's schemaless nature means there is no DDL to enforce consistency — schema drift between consumers is possible and difficult to detect until runtime failure.

**Option C — Proxy as data model authority**

The proxy owns and is the sole writer of the policy store schema. The Next.js application reads policy data through the proxy's API.

*Advantages:* Single schema owner eliminates drift risk.
*Risks:* Expands the proxy's scope; requires refactoring the Next.js application; tightly couples front-end and proxy deployment cycles.

**Hybrid option** — Direct DynamoDB for high-frequency per-request lookups (bucket flags, user grants); REST API for management operations (issuer registration, role updates).

### Workers Caching Stack

For the Workers deployment:

- **In-process cache** — per-isolate, not shared across edge nodes, with TTLs from ADR-005
- **Workers KV** — eventually consistent, globally distributed key-value store; serves as a shared cache tier that survives isolate recycling

For access control decisions, eventual consistency is generally acceptable — a grant created seconds ago but not yet visible in KV is a minor inconvenience, not a security failure.

### Unresolved

- The implementation choice between Options A, B, C, and the hybrid is the primary open question. See RFC-001 Open Question 2.
- The full caching stack for Workers (which lookups use Workers KV vs. in-process only, cache warming strategy for cold isolates) requires further design.

---

## Consequences

**Benefits**

- Per-request policy resolution enables dynamic permissions without token re-exchange
- In-process caching absorbs the majority of lookup load
- Workers KV provides a shared cache tier for the edge deployment
- The configuration layer is behind a trait interface, allowing different implementations per deployment

**Costs / Risks**

- The policy store is a single point of failure for authenticated requests to non-public resources
- Cache misses on cold Workers isolates add latency to the first request
- Schema governance between the proxy and Next.js application is a risk regardless of implementation choice
- The implementation decision is blocked pending team discussion

---

## Alternatives Considered

**Encode permissions in the session token (no policy store on hot path)** — rejected. Freezes permissions at exchange time. Users would need to re-exchange tokens after any permission change. See ADR-005.

**Global strongly-consistent cache (e.g. Durable Objects)** — considered. Would eliminate eventual-consistency concerns. Rejected: Durable Objects are single-region, adding latency for global edge requests. Eventual consistency is acceptable for the access control use case.

**Push-based cache invalidation** — considered. The policy store pushes updates to Workers KV when grants change, rather than relying on TTL-based expiry. Worth exploring as an optimisation but adds operational complexity. Deferred.
