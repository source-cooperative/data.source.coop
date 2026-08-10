# ADR-005: Authorization — Delegated to the Source Cooperative API

**Status:** Accepted — implemented
**Date:** 2026-03-14
**RFC:** RFC-001 §8
**Depends on:** ADR-001, ADR-004
**Implementation:** `src/authz.rs`, `src/source_api/registry.rs`, `src/source_api/auth.rs`; `source.coop:src/lib/api/oidc.ts`
**Implemented by:** #116 (registry + API resolution), #149 (product visibility model), #162 (authorize and enable writes), #170 (extract `decide_backend_auth` + CI ordering test), #183 (hermetic API stub, contract and failure-mode tests) · source.coop#283 (OIDC auth), source.coop#284 (require auth for restricted products)

---

## Context

ADR-004 issues a session credential carrying the caller's original OIDC subject. This ADR defines how the proxy decides whether a given request on a given product is permitted.

The RFC proposed that the proxy evaluate an intersection locally: a permission ceiling embedded in the session token, intersected with the account's live grants from the policy store. That design depends on Roles carrying permission statements, which do not exist yet (ADR-010). What shipped instead is simpler and, for a single unlimited Role, equivalent: **the proxy does not evaluate policy at all — it delegates the decision to the Source Cooperative API by making every lookup on behalf of the caller.**

---

## Decision

### Subject-Scoped Lookups Are the Authorization Mechanism

The proxy resolves each request by calling the Source Cooperative API *as the caller*. Every lookup carries a short-lived JWT whose `sub` is the caller's identity, and the API applies exactly the same permission resolution it applies to a request from the frontend.

Authorization is therefore a **property of the resolution path, not a separate check**: if the caller is not entitled to a product or its data connection, the subject-scoped fetch returns 404 or 403, which propagates as `BucketNotFound` or `AccessDenied` before the proxy ever reaches the backend. An anonymous caller sends no subject and sees only what the API serves unauthenticated.

This ordering is enforced by data dependency rather than by statement order: the connection value only exists once its subject-scoped fetch has succeeded, so an unauthorized caller cannot reach backend federation. That property is covered by unit tests on `decide_backend_auth` and end-to-end by `test_restricted_product_denied_to_anonymous`.

### Request Resolution

1. **Identify the caller.** Unsealing the session token yields `source_identity`, the original OIDC subject. Absent credentials mean anonymous.
2. **Map the request.** `/{account}/{product}/{key}` is rewritten to an internal `account:product` bucket.
3. **Fetch the product** (subject-scoped, cached 300s). A caller not entitled to it gets 404/403.
4. **Fetch the referenced data connection by id** (subject-scoped, cached 300s). Resolving by id — rather than scanning a cached list — is what lets the API authorize this exact resource.
5. **For writes only, fetch the caller's permissions** on the product (subject-scoped, cached 60s). Reads never consult them.
6. **Decide, then federate.** `decide_backend_auth` applies the write gate and only then translates the connection's backend authentication into backend options.

### The Write Gate

Reads require nothing beyond a successful subject-scoped resolution. A write additionally requires all of:

- **An authenticated caller.** Anonymous callers can never write — there is no subject to resolve permissions with.
- **The `write` permission** on the product, from the API's `/permissions` endpoint.
- **A connection that is not `read_only`.**
- **A connection the proxy can actually sign as** — in practice an S3 web-identity role (ADR-006). An unsigned or unsupported connection cannot accept writes regardless of the caller's permissions.

Write actions are classified by a **denylist** over the closed action set: everything that is not `GetObject`, `HeadObject`, or `ListBucket` is treated as a write. This is fail-safe by direction — a new read-only action added upstream would be harmlessly gated as a write until classified, never the reverse.

On every denial, backend options are left untouched. An unauthorized request must never have credentials or `skip_signature` emitted on its behalf.

### Proxy-to-API Authentication

The proxy authenticates each policy-store lookup as the caller, so the API needs no separate service-account code path.

| Claim | Value |
|---|---|
| `sub` | The caller's identity from the session token |
| `iss` | The proxy's OIDC issuer URL |
| `aud` | The API's origin |
| `exp` | Short-lived |

The API verifies the token against the proxy's published JWKS (`/.well-known/jwks.json`), pinning **RS256**, the expected issuer, and the expected audience, with a 30-second clock tolerance. An `Authorization` header that is present but invalid **must not** fall back to cookie authentication — otherwise a bogus Bearer token would silently succeed via a session cookie.

The API trusts the proxy to assert any `sub`. That trust rests on the JWT signature: only the proxy holds the signing key. This is the same shape as an AWS service assuming an IAM role on a principal's behalf.

> [!NOTE]
> **The subject is an individual identity, not an account.** The proxy signs with the caller's Ory identity id, and the API resolves it through the identity index. Organisation accounts are never the subject of a proxy-issued token. The RFC anticipated `sub` = `account_id`, which "may be a user or an organisation", to support a CI workflow assuming an org-owned Role. That path arrives with ADR-010; until then there is no Role for an organisation to own.

### Batch Delete

Per-key authorization for batch delete confirms only that the operation is a write, relying on the product-level authorization already performed during resolution. This is sufficient because Source Cooperative authorizes writes at the product level, and defensible as defence in depth — it is only reached for write batch operations and never blanket-allows a read. It would be insufficient if a future multistore invoked it without a prior successful resolution for the same bucket.

### Caching

All lookups are cached in the Cloudflare Cache API, keyed on the API URL plus the caller's subject so that one caller's authorized response can never serve another. See ADR-007 for TTLs and their rationale.

### Anonymous Access

Anonymous reads need no exchange, no credentials, and no Role — `--no-sign-request` works. The proxy accepts anonymous S3 requests by design (`anonymous_access: true` on every resolved bucket) because callers are authorized by the Source Cooperative API upstream, not by signing to the proxy.

---

## Consequences

**Benefits**

- **One permission model, one implementation.** The API is the single authority; the proxy cannot drift from the frontend's answer, because it asks the same question through the same code path.
- Permissions are always live — no re-exchange after a new grant or an organisation membership change, bounded only by cache TTL.
- The confused-deputy failure mode is closed structurally: federation is unreachable without a successful authorized resolution.
- Anonymous public reads stay frictionless and need no account lookup.
- Substantially less security-critical code in the proxy than a local policy evaluator would require.

**Costs / Risks**

- **Every request depends on the API.** It is on the hot path for cache misses, and its availability bounds the proxy's.
- **There is no ceiling.** A credential carries the caller's full permissions; a caller cannot obtain a narrower one. Scoped access awaits ADR-010 and ADR-011.
- Authorization granularity is whatever the API exposes: product-level `read`/`write`. There is no prefix confinement, and no per-object grant.
- Cache TTL is an authorization-revocation lag, not merely a freshness knob — a revoked write grant remains effective for up to 60 seconds, and a connection flipped to read-only for up to 5 minutes.
- Organisation-owned automation has no representation; every credential is an individual's.

---

## Alternatives Considered

**Role ceiling intersected with account permissions, evaluated in the proxy** — the original RFC-001 §8 design, deferred rather than rejected. It requires Roles carrying permission statements, and delivers nothing while `_default` is the only Role and its ceiling is unlimited: the intersection would be the account's permissions in every case. Specified in ADR-011 and gated on ADR-010.

**Encode full permissions in the session token** — rejected. Freezes permissions at exchange time; users would need to re-exchange after every permission change. Unacceptable on a platform where users create datasets and join organisations continuously.

**A service-account identity for proxy-to-API calls** — rejected. Would require the API to grow a parallel authorization path for "the proxy acting on behalf of X", with its own escalation risks. Presenting as the caller reuses the frontend's path exactly.

**Falling back to cookie authentication when a Bearer token fails** — rejected as unsafe, and explicitly guarded against: it would let an invalid token succeed by way of an ambient session.

**Evaluating organisation membership and permission inheritance in the proxy** — rejected. The API resolves inherited grants internally and returns the account's effective permissions; duplicating that logic would create two sources of truth for the platform's most security-sensitive computation.
