# ADR-007: Configuration Layer — Policy Store Implementation and Caching Strategy

**Date:** 2026-03-14
**RFC:** RFC-001 §11
**Depends on:** ADR-004, ADR-005
**Language:** ASD-STE100 Simplified Technical English

---

## Context

The authorization model (ADR-005) makes the proxy look in a policy store for each authenticated request that is not public. The STS exchange (ADR-004) makes the proxy look for the Role definition and the IdP record when it issues a token. Thus there are two different hot paths: the authorization of each S3 request, and the STS exchange of each session. The policy store must serve both with acceptable latency and availability.

---

## Decision

### Managed Entities

The policy store holds these entities:

| Entity | Owner | Written by | Read by |
|--------|-------|-----------|---------|
| **Product metadata** (public flag, backend config) | Platform | Next.js app or proxy (TBD) | Proxy (per-request) |
| **Account permission grants** | Platform | Next.js app or proxy (TBD) | Proxy (per-request) |
| **Role definitions** (identity constraints, permission statements) | Account owner | Management API (TBD) | Proxy (STS exchange) |
| **Platform IdP records** (issuer URL, well-known claims) | Platform operator | Configuration / deployment | Proxy (STS exchange) |

ADR-004 specifies the management API for the Roles: `/api/accounts/{account_id}/roles`. We have not yet decided which component serves this API. It can be the proxy, the Next.js application, or a separate service. This decision depends on the implementation choice below.

### Access Patterns

The proxy accesses the configuration in three different ways.

**Frequent and sensitive to latency (for each S3 request)**
- Product public flag — `product_id → {public, backend_config}`
- Account permission — `(account_id, product_id) → {granted, prefix_restrictions}`
- Account product list — `account_id → [product_ids]`

These lookups must complete in less than 10 milliseconds. The in-process cache absorbs most of the load. But a cache miss must also be fast.

**Less frequent and sensitive to latency (for each STS exchange)**
- Role definition — `(account_id, role_name) → Role`
- Platform IdP record — `idp_id → IdP`

An STS exchange occurs one time for each session, and not for each request. But it is on the critical path to start a session. With a cache (30–60 s TTL), these lookups must also complete in less than 10 milliseconds.

**Infrequent management (in the background)**
- Fetch and refresh of the issuer JWKS (1 hour TTL, stale copy if the fetch fails)
- Rotation of a provider credential
- Create, read, update, and delete operations on a Role

These can take more time. They are on no hot path.

### `backend_config`

The product metadata record contains a `backend_config` object. This object connects the authorization (ADR-005) to the outbound storage (ADR-006):

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

The `credential_ref` field identifies an OIDC trust relation or a stored credential secret (refer to ADR-006). The storage backend resolver trait of multistore specifies the exact schema.

### Implementation Approach

The proxy calls the existing Source Cooperative API for each lookup. Two cache layers protect the API: an in-process cache in each isolate with a short TTL, and Workers KV as a shared distributed cache.

The Next.js application stays the only owner of the schema. The proxy needs no direct database credentials. The API applies the schema constraints before the data comes to the proxy. The Next.js application also serves the management APIs for the Roles and the IdPs.

The REST API is an availability dependency on the hot path when the cache misses. The in-process cache absorbs most of the lookups. If measurements show that the API is too slow, we can add direct DynamoDB access for the most frequent lookups, which are the product flags and the account grants. The management operations stay on the API.

### Cache Strategy

The proxy caches each lookup in its own process (one cache for each isolate).

| Lookup | Cache Key | TTL | Notes |
|--------|-----------|-----|-------|
| Product public flag | `product_id` | 60–300s | Rarely changes |
| Account permission for product | `(account_id, product_id)` | 30–60s | Reflects grants, org membership |
| Account's full product list | `account_id` | 5–10s | Freshness-sensitive for UI |
| Role definition | `(account_id, role_name)` | 30–60s | Changes infrequently |
| JWKS | `issuer_url` | 1 hour | Stale-while-revalidate on failure |

### Cache Stack in Workers

The Workers deployment has two cache tiers:

- **In-process cache** — it belongs to one isolate, the edge nodes do not share it, and it uses the TTLs above.
- **Workers KV** — it is eventually consistent and globally distributed. It is available as a shared cache for the policy data, and it continues to exist after an isolate stops.

Eventual consistency is satisfactory for access control decisions. A grant that is a few seconds old and not yet visible in KV is a small inconvenience and not a security failure.

### Open Questions

- The full cache stack for Workers. Which lookups use Workers KV, and which lookups use only the in-process cache? How do we fill the cache when an isolate starts cold?
- How the system makes the `_default` Role. Does it make the Role at runtime (recommended), or does it write the Role to the storage when it makes the account?

---

## Consequences

**Benefits**

- The policy resolution for each request gives dynamic permissions. A caller does not do a new token exchange.
- The in-process cache absorbs most of the lookup load.
- Workers KV gives a shared cache tier to the edge deployment.
- A trait interface hides the configuration layer. Thus each deployment can use a different implementation.
- The document lists each managed entity with its owner and its access pattern.

**Costs and Risks**

- The REST API is an availability dependency on the hot path when the cache misses.
- A cache miss on a cold Workers isolate adds latency to the first request.
- If the API becomes a bottleneck, we move the most frequent lookups to direct DynamoDB access. Then the schema governance needs strict discipline.

---

## Alternatives Considered

**Put the permissions in the session token, with no policy store on the hot path** — rejected. This freezes the permissions at the exchange. A user must then do a new exchange after each permission change. The current design puts the Role ceiling in the token, and thus removes one lookup. But the account permissions stay dynamic. Refer to ADR-005.

**A global and strongly consistent cache, for example Durable Objects** — considered. This removes the questions about eventual consistency. Rejected: Durable Objects operate in one region, and this adds latency to a global edge request. Eventual consistency is satisfactory for access control.

**Direct DynamoDB access** — considered. This removes the availability dependency on the REST API and gives reads in less than 10 milliseconds. Rejected as the first approach: two systems, the proxy and Next.js, then write to the same DynamoDB tables. This makes a schema governance problem, and such a problem is difficult to find before a failure at runtime. We can add direct access later for specific frequent lookups, if the measurements show that the API is a bottleneck.

**The proxy as the owner of the data model** — considered. The proxy then owns the policy store schema, and the Next.js application reads through the API of the proxy. Rejected: this makes the scope of the proxy much larger, and it connects the deployment cycles of the frontend and the proxy too tightly.

**Push-based cache invalidation** — considered. The policy store sends an update to Workers KV when a grant changes, and the cache does not wait for a TTL. This is a good optimisation, but it adds operational complexity. Deferred.
