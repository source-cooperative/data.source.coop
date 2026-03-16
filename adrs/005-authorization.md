# ADR-005: Authorization Model — Dynamic Per-Request Policy Resolution

**Status:** Pending
**Date:** 2026-03-14
**RFC:** RFC-001 §8
**Depends on:** ADR-001, ADR-004

---

## Context

ADR-001 establishes that session tokens are stateless JWTs encoding identity and role, but **not** permissions. This ADR defines how permissions are resolved at request time.

Two properties drive the design:

1. **Permissions are dynamic.** A user who creates a new organisation or dataset should be able to access it immediately. Encoding permissions in the session token would freeze them at exchange time, requiring re-exchange to reflect changes.

2. **The role is a ceiling; user permissions are the grants.** The role answers "what classes of action are permitted for this identity type?" The per-user permission lookup answers "which specific resources can this identity access?" The proxy enforces the intersection.

This mirrors AWS IAM: a session token asserts role membership, and the role's current policies are evaluated live on each API call.

---

## Decision

### Identity Model

The session token carries three fields relevant to authorization:

- `user_id` — stable identifier for the authenticated principal
- `role_id` — one of: `anonymous`, `authenticated_user`, `admin`
- `exp` — token expiry; checked before any policy evaluation

### Role Definitions

**`anonymous`**
- Permitted action classes: read-only (`GetObject`, `HeadObject`, `ListObjects`, `ListBuckets`)
- Role-level filter: only buckets flagged `public = true` are visible and accessible
- No user permission lookup — role filter is the only guard

**`authenticated_user`**
- Permitted action classes: read and write (`GetObject`, `PutObject`, `HeadObject`, `DeleteObject`, `ListObjects`, `ListBuckets`, `CreateBucket`)
- Role-level filter: none — user permission lookup determines which resources are accessible
- User permission lookup is always performed for non-public resources

**`admin`**
- Permitted action classes: all
- Role-level filter: none
- User permission lookup: **skipped** — admin role has unconditional access to all resources
- Admin role assumption is gated on strong identity claims (e.g. Ory group membership) to prevent accidental privilege escalation

### Per-Request Resolution Strategy

Authorization proceeds in at most three steps, with early exits to minimise unnecessary lookups:

**Step 1 — Role action check**
Does this role permit the requested action class? If the role is `anonymous` and the request is a `PutObject`, deny immediately. This is pure in-memory logic against the role definition — no lookup required.

**Step 2 — Public resource early exit**
For read operations only: is the requested bucket flagged `public = true` in the policy store? If yes, and the role permits reads, permit immediately without a user permission lookup. This covers the majority of Source Cooperative traffic (public dataset access) and avoids a per-user lookup for every read of public data.

**Step 3 — User permission lookup**
For non-public resources or write operations: fetch the user's permissions from the policy store and evaluate them against the requested resource. This reflects current organisation membership, dataset ownership, and explicit grants.

### Operation-Specific Behaviour

**Single-resource operations (`GetObject`, `PutObject`, `HeadObject`, `DeleteObject`)**
After the role check and public early exit, a point lookup: does `(user_id, bucket_id)` resolve to an access grant? If the grant includes prefix restrictions, those are enforced against the requested object key.

**`ListBuckets`**
The proxy constructs this response entirely from the policy store — the upstream is never called. Anonymous users see `public = true` buckets; authenticated users see all buckets they have grants for; admins see all buckets.

**`ListObjects` (within a bucket)**
After the role check, public early exit, and user permission lookup: if the grant includes a key prefix restriction, it is passed as a filter to the upstream `ListObjects` call so the upstream enforces the boundary.

### Cache Strategy

All policy store lookups are cached in-process (Workers: per-isolate; ECS: per-container).

| Lookup | Cache Key | TTL |
|---|---|---|
| Role definition | `role_id` | In-memory constant |
| Bucket public flag | `bucket_id` | 60–300s |
| Single-resource user grant | `(user_id, bucket_id)` | 30–60s |
| User's full bucket list (`ListBuckets`) | `(user_id, role_id)` | 5–10s |

The short TTL on the full bucket list ensures that a user who creates a new dataset sees the change within seconds. For Workers, cache is per-isolate and not shared across edge nodes; Workers KV is available as a shared tier if needed.

### Unresolved: Grant Schema

The exact schema of user access grants is unresolved. Open questions include:

- Whether grants are bucket-level only or support sub-bucket prefix granularity
- Whether grants are additive (allow-only) or support explicit denies
- How organisation membership is modelled — derived grants from membership, or explicit per-bucket grants per member

These questions are tracked in RFC-001 Open Question 7.

---

## Consequences

**Benefits**

- Permissions reflect current state — no re-exchange required after creating a new dataset or joining an organisation
- The majority of traffic (public dataset reads) resolves with no user-specific lookup
- Admin bypass eliminates unnecessary lookups for administrative operations
- Cache TTLs are tuned per-operation to balance freshness and performance
- The model is familiar to anyone who knows AWS IAM

**Costs / Risks**

- Every non-public authenticated request requires a policy store lookup (mitigated by caching)
- The policy store is on the hot path — its availability affects request latency for cache misses
- Per-isolate caching in Workers means cache is not shared across edge nodes (cold isolate = cache miss)
- Grant schema is unresolved — the implementation cannot begin until the schema is defined

---

## Alternatives Considered

**Encode permissions in the session token** — rejected. Freezes permissions at exchange time. Users would need to re-exchange tokens to see permission changes. Unacceptable for a platform where users create datasets and join organisations dynamically.

**Centralised permission cache (Redis / Workers KV as primary)** — considered. Would share cache across isolates and containers. Rejected as the primary tier: adds a network hop to every cache read. Per-isolate caching with optional Workers KV as a secondary tier is preferred.

**Explicit deny support in grants** — deferred. Additive (allow-only) grants are simpler to reason about and sufficient for the initial use cases. Explicit denies can be added later if the access control model requires it.
