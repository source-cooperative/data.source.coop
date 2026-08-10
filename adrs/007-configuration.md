# ADR-007: Configuration Layer — Policy Store and Caching Strategy

**Status:** Accepted — implemented
**Date:** 2026-03-14
**RFC:** RFC-001 §11
**Depends on:** ADR-004, ADR-005
**Implementation:** `src/source_api/cache.rs`, `src/source_api/registry.rs`, `src/config.rs`
**Implemented by:** #116 (Source API as the configuration source), #160 (percent-encoded cache-key subjects), #183 (hermetic API stub for CI) · source.coop#283 (OIDC auth on the API)

---

## Context

The authorization model (ADR-005) requires per-request lookups against a policy store for every request. The policy store must serve that hot path with acceptable latency and availability.

---

## Decision

### The Source Cooperative API Is the Policy Store

The proxy calls the existing Source Cooperative REST API for all configuration lookups. The Next.js application remains the **sole schema owner**; the proxy holds no database credentials and never reads the underlying tables directly. The API enforces schema constraints before data reaches the proxy.

This keeps the operational surface small and avoids the schema-governance problem that arises when two systems read the same database.

| Lookup | Endpoint |
|---|---|
| Product metadata | `/api/v1/products/{account}/{product}` |
| Product list for an account | `/api/v1/products/{account}` |
| Data connection by id | `/api/v1/data-connections/{id}` |
| Caller's permissions on a product | `/api/v1/products/{account}/{product}/permissions` |

All are fetched with the caller's identity (ADR-005), so the API's response *is* the authorization decision.

### Caching — the Cloudflare Cache API

Responses are cached in the **Cloudflare Cache API**, not an in-process map and not Workers KV. This is the significant departure from the RFC, and it is an improvement: the Cache API is shared across all isolates in a colo, so it survives isolate recycling and warms once per location rather than once per isolate.

| Lookup | TTL | Rationale |
|---|---|---|
| Product metadata | 300s | Subject-authorized, so the TTL is an authorization-revocation lag |
| Data connection | 300s | Fetched on *every* request; a short TTL taxes the read path |
| Product list | 60s | Freshness-sensitive for listings |
| Caller's permissions | 60s | Gates writes — a revoked grant must stop taking effect quickly |
| Issuer JWKS | 900s | Isolate-shared, separate from the Cache API |

**Cache keys are scoped by subject.** Each key combines the API URL with the caller's identity, so an authorized response can never be served to a different caller. Path segments are percent-encoded against an RFC 3986 unreserved set before entering a URL, so a decoded `?`, `#`, `&`, `/`, or space in an account or product slug can neither inject into the upstream URL nor forge a colliding key.

The asymmetry between the connection TTL (300s) and the permission TTL (60s) is deliberate. Flipping a connection to read-only freezes *all* writers and is a deliberate administrative act where several minutes of lag is acceptable; revoking one compromised account's write grant is the urgent case, and it rides the 60-second permission TTL.

### Availability

The API is on the hot path for cache misses, so its availability bounds the proxy's for anything not currently cached. Cache hits are served without contacting it.

For CI, the API is stubbed (`tests/stub_api.py`) so the test suite is hermetic and does not depend on a live control plane, while object reads still exercise real buckets.

---

## Consequences

**Benefits**

- Per-request resolution means permissions are always live — no token re-exchange after a grant changes.
- One schema owner; no dual-writer governance problem.
- Colo-shared caching absorbs the majority of lookups and survives isolate recycling.
- Subject-scoped keys make cross-caller cache poisoning structurally impossible.

**Costs / Risks**

- The REST API is an availability dependency for cache misses on the hot path.
- Cache misses in a cold colo add latency to the first request there.
- **TTLs are revocation lags, not just freshness knobs.** Up to 60 seconds for a revoked write grant, up to 5 minutes for a product or connection change.
- No push-based invalidation: a change cannot be made to take effect faster than its TTL.

---

## Alternatives Considered

**In-process (per-isolate) cache backed by Workers KV** — the RFC's design; superseded. The Cache API achieves the shared-tier goal with no extra service, no eventual-consistency window to reason about, and no additional binding. Workers KV remains available if a cross-colo tier is ever wanted.

**Encode permissions in the session token (no policy store on the hot path)** — rejected. Freezes permissions at exchange time. See ADR-005.

**Global strongly-consistent cache (Durable Objects)** — considered. Rejected: single-region, adding latency for global edge requests. Eventual consistency is acceptable for access control.

**Direct DynamoDB access** — considered. Would eliminate the REST API dependency and give single-digit millisecond reads. Rejected: two systems on the same tables creates a schema-governance problem that is hard to detect until runtime failure. Can be introduced later for specific high-frequency lookups if profiling shows the API is the bottleneck.

**Proxy as data model authority** — rejected. Significantly expands the proxy's scope and couples frontend and proxy deployment cycles.

**Push-based cache invalidation** — deferred. Would let a grant change take effect immediately rather than at TTL expiry, at the cost of operational complexity. Worth revisiting if the revocation lag proves unacceptable.
