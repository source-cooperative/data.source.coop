# ADR-009: Configuration Layer — Policy Store Implementation and Caching Strategy

**Status:** Proposed
**Date:** 2026-03-14
**RFC:** RFC-001 §12
**Depends on:** ADR-004, ADR-005

---

## Context

The authorization model (ADR-005) requires per-request lookups against a policy store for every non-public authenticated request. The STS exchange (ADR-004) requires lookups for Role definitions and IdP records during token issuance. Together, these create two distinct hot paths: per-S3-request authorization and per-session STS exchange. The policy store must serve both with acceptable latency and availability.

---

## Decision

### Managed Entities

The policy store manages the following entities:

| Entity | Owner | Written by | Read by |
|--------|-------|-----------|---------|
| **Product metadata** (public flag, backend config) | Platform | Next.js app or proxy (TBD) | Proxy (per-request) |
| **Account permission grants** | Platform | Next.js app or proxy (TBD) | Proxy (per-request) |
| **Role definitions** (identity constraints, permission statements) | Account owner | Management API (TBD) | Proxy (STS exchange) |
| **Platform IdP records** (issuer URL, well-known claims) | Platform operator | Configuration / deployment | Proxy (STS exchange) |

The management API for Roles (`/api/accounts/{account_id}/roles`) is defined in ADR-004. Which component serves this API — the proxy, the Next.js application, or a dedicated service — is unresolved and tied to the implementation choice below.

### Access Patterns

The proxy's configuration access has three distinct profiles:

**High-frequency, latency-sensitive (per S3 request)**
- Product public flag lookup — `product_id → {public, backend_config}`
- Account permission lookup — `(account_id, product_id) → {granted, prefix_restrictions}`
- Account's full product list — `account_id → [product_ids]`

These must complete in single-digit milliseconds. In-process caching absorbs most of the load; the underlying lookup must be fast for cache misses.

**Medium-frequency, latency-sensitive (per STS exchange)**
- Role definition lookup — `(account_id, role_name) → Role`
- Platform IdP record lookup — `idp_id → IdP`

STS exchanges happen once per session (not per request), but they are on the critical path for session establishment. Role and IdP lookups should complete in single-digit milliseconds with caching (30–60s TTL).

**Low-frequency, management (background)**
- Issuer JWKS fetch and cache refresh (1hr TTL, stale-while-revalidate)
- Provider credential rotation
- Role CRUD operations

These tolerate higher latency and are not on any request hot path.

### `backend_config`

The product metadata record includes a `backend_config` that bridges authorization (ADR-005) and outbound storage (ADR-006):

```json
{
  "public": true,
  "backend_config": {
    "storage_url": "s3://provider-bucket/prefix/",
    "credential_ref": "oidc-trust-provider-x",
    "region": "us-west-2"
  }
}
```

The `credential_ref` identifies either an OIDC trust relationship or a stored credential secret (see ADR-006). The exact schema is defined by the `proxy-storage` crate's backend resolver trait (ADR-008).

### Implementation Approach

The proxy calls the existing Source Cooperative API for all lookups, wrapped in multi-layer caching: in-process (per-isolate) with short TTL, backed by Workers KV as a shared distributed cache tier.

The Next.js application remains the sole schema owner. The proxy does not need direct database credentials. The API enforces schema constraints before data reaches the proxy. Management APIs for Roles and IdPs are served by the Next.js app.

The REST API is an availability dependency on the hot path for cache misses. In-process caching absorbs the majority of lookups. If profiling reveals the API as a latency bottleneck, direct DynamoDB access can be introduced for the highest-frequency lookups (product flags, account grants) while keeping management operations on the API.

### Cache Strategy

All lookups are cached in-process (per-isolate):

| Lookup | Cache Key | TTL | Notes |
|--------|-----------|-----|-------|
| Product public flag | `product_id` | 60–300s | Rarely changes |
| Account permission for product | `(account_id, product_id)` | 30–60s | Reflects grants, org membership |
| Account's full product list | `account_id` | 5–10s | Freshness-sensitive for UI |
| Role definition | `(account_id, role_name)` | 30–60s | Changes infrequently |
| JWKS | `issuer_url` | 1 hour | Stale-while-revalidate on failure |

### Workers Caching Stack

For the Workers deployment:

- **In-process cache** — per-isolate, not shared across edge nodes, with TTLs above
- **Workers KV** — eventually consistent, globally distributed; available as a shared cache tier for policy data that survives isolate recycling

For access control decisions, eventual consistency is generally acceptable — a grant created seconds ago but not yet visible in KV is a minor inconvenience, not a security failure.

### Unresolved

- The full caching stack for Workers (which lookups use Workers KV vs. in-process only, cache warming strategy for cold isolates).
- How the `_default` Role is provisioned — synthesized at runtime (recommended) or materialized in storage when accounts are created.

---

## Consequences

**Benefits**

- Per-request policy resolution enables dynamic permissions without token re-exchange
- In-process caching absorbs the majority of lookup load
- Workers KV provides a shared cache tier for the edge deployment
- The configuration layer is behind a trait interface, allowing different implementations per deployment
- All managed entities are explicitly cataloged with ownership and access patterns

**Costs / Risks**

- The REST API is an availability dependency for cache misses on the hot path
- Cache misses on cold Workers isolates add latency to the first request
- If the API proves to be a bottleneck, migrating high-frequency lookups to direct DynamoDB access will require schema governance discipline

---

## Alternatives Considered

**Encode permissions in the session token (no policy store on hot path)** — rejected. Freezes permissions at exchange time. Users would need to re-exchange tokens after any permission change. The current design embeds the Role ceiling in the token (avoiding one lookup) while keeping account permissions dynamic. See ADR-005.

**Global strongly-consistent cache (e.g. Durable Objects)** — considered. Would eliminate eventual-consistency concerns. Rejected: Durable Objects are single-region, adding latency for global edge requests. Eventual consistency is acceptable for the access control use case.

**Direct DynamoDB access** — considered. Eliminates the REST API availability dependency and provides single-digit millisecond reads. Rejected as the initial approach: two systems (proxy and Next.js) accessing the same DynamoDB tables creates a schema governance problem that is difficult to detect until runtime failure. Can be introduced later for specific high-frequency lookups if profiling indicates the API is a bottleneck.

**Proxy as data model authority** — considered. The proxy owns the policy store schema and the Next.js application reads through the proxy's API. Rejected: significantly expands the proxy's scope and tightly couples front-end and proxy deployment cycles.

**Push-based cache invalidation** — considered. The policy store pushes updates to Workers KV when grants change, rather than relying on TTL-based expiry. Worth exploring as an optimisation but adds operational complexity. Deferred.
