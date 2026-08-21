# ADR-011: Role-Ceiling Authorization

**Status:** Proposed — not implemented
**Date:** 2026-08-09
**RFC:** RFC-001 §8
**Depends on:** ADR-005, ADR-010

---

## Context

ADR-005 delegates every authorization decision to the Source Cooperative API through subject-scoped lookups. With one unlimited Role that is exactly right: there is no ceiling to intersect, so a local check would be a no-op.

Once Roles carry permission statements (ADR-010), that stops being true. A credential issued for a scoped Role must be constrained by that Role **even when the account behind it has broader permissions** — otherwise the ceiling is decorative and least privilege is unenforced.

This ADR specifies the request-time evaluation that makes a Role ceiling real. It is the RFC-001 §8 design, deferred until it has something to enforce.

---

## Decision

### Two Properties Drive the Design

1. **The Role is a ceiling; account permissions are the grants.** The Role's permission statements, sealed into the session token at exchange time, answer "what is the maximum scope of these credentials?" The per-account lookup answers "what can this account actually access?" The proxy enforces the **intersection**. A Role can narrow access but never widen it beyond what the account has.

2. **Account permissions stay dynamic.** A user who joins an organisation or receives a grant on a new dataset sees it reflected without re-exchanging credentials, because account permissions are resolved per request rather than frozen in the token.

This mirrors AWS IAM: the session token asserts role membership with an embedded permission boundary, and the role's current policies are evaluated live on each call.

### Carrying the Ceiling in the Token

The sealed session token (ADR-001) already has a slot for this: `allowed_scopes`, sealed at mint time and currently empty. The Role's permission statements populate it.

Sealing at mint time is what makes the ceiling check **local** — no network call before the first denial. It also means a Role edit does not retroactively narrow credentials already issued; they expire out. That is the same trade AWS makes with session policies, and it is why `max_session_duration` matters (ADR-010).

> [!NOTE]
> If the token format is revisited (ADR-001 Alternatives), this is the requirement that should drive it: the ceiling and an `assumed_by` audit subject are exactly the claims the ES256-JWT design carried and the sealed token does not expose.

### Per-Request Resolution

The delegated path in ADR-005 gains a ceiling check ahead of it. Each step can deny immediately:

**Step 1 — Identify the caller.** No credentials → anonymous; only reads of public products. Otherwise unseal the session token.

**Step 2 — Role ceiling check (local, no network).** Match the requested action and resource against the token's sealed permission statements. No match → deny. This is a local check against data already in the token.

**Step 3 — Resource resolution.** Map the S3 request to `account_id/product_name` plus an object key.

**Step 4 — Public resource early exit.** For reads of public products, permit. This keeps the majority of traffic on the fast path.

**Step 5 — Account permission lookup.** Subject-scoped fetch as in ADR-005. The effective permission is `(Role ceiling) ∩ (account permissions)`.

**Step 6 — Prefix enforcement.** If a matched statement carries a prefix constraint, verify the object key falls within it. Evaluated as part of the resource matching in steps 2 and 5.

The proxy does not evaluate organisation membership or permission inheritance — the API resolves those internally and returns the account's effective grants (ADR-005).

### Statement Matching

1. **Action match** — does the statement's `actions` include the requested class (`read` or `write`)?
2. **Resource match** — does any pattern in `resources` match the requested resource?
   - `*` matches everything
   - `sc::{account}::product/{name}` and `.../{name}/*` match the whole product
   - `sc::{account}::product/{name}/{prefix}/*` matches objects under a prefix
   - `sc::{account}::product/{name}/{key}` matches a single object

If any statement matches both action and resource, the Role permits it; the account lookup then decides whether the underlying access exists.

### Operation-Specific Behaviour

**Single-resource operations** — after the ceiling check and public early exit, a point lookup, with prefix restrictions from both Role and account grant enforced against the key.

**`ListBuckets`** — constructed from the policy store; the upstream is never called. Anonymous callers see public products; an unlimited ceiling sees everything the account is granted; a scoped Role sees only the intersection of its resource patterns with the account's grants. **This is currently `unimplemented!()`** and would be built here.

**`ListObjects`** — if the Role carries a key prefix restriction, pass it as a filter to the upstream call rather than filtering after the fact.

### Denial Semantics

A denied request returns `403 Forbidden` with a standard S3 error body that **does not distinguish** between the Role ceiling, missing account permissions, and a nonexistent product — otherwise the response leaks resource existence to unauthorized callers.

For `ListBuckets` and `ListObjects`, results are filtered silently rather than erroring: the caller sees only what they can access.

### Audit Logging

Every request with STS credentials should log account, Role, the original IdP subject, action, resource, and result — attribution that survives credentials acting as the account. This depends on the token carrying an `assumed_by` subject; see the note above.

---

## Consequences

**Benefits**

- A Role ceiling becomes enforceable, so least privilege is real rather than advisory.
- The first authorization check is local — a scoped credential's denials cost no network call.
- Account permissions stay live; no re-exchange after a grant changes.
- Prefix-scoped write access ("this CI job may write only under `uploads/`") becomes expressible.
- `ListBuckets` stops being unimplemented.

**Costs / Risks**

- **Two authorization mechanisms now exist** — the local ceiling and the delegated API decision — and they must agree on what `read` and `write` mean. Divergence between the proxy's action classification and the API's permission model is a silent correctness bug in the platform's most sensitive path.
- Pattern matching over URNs is security-critical parsing; prefix and wildcard handling is a classic source of confused-deputy bugs.
- Sealing the ceiling means a tightened Role does not take effect until issued credentials expire.
- `ListBuckets` from the policy store makes an unbounded-fan-out lookup out of what is otherwise a cheap call.
- The permission model stays additive (allow-only); expressing "everything except X" requires enumerating everything except X.

---

## Alternatives Considered

**Keep pure delegation and let the API enforce the ceiling** — considered. The proxy would forward the Role with each lookup and the API would return the intersection. Attractive: one evaluator, no duplicated semantics. Rejected for the hot path — it puts a policy round trip in front of every request including denials, and gives up the local fast path that makes a scoped credential cheap. Reconsider if the two mechanisms prove hard to keep consistent; correctness would outweigh the latency.

**Encode full account permissions in the token** — rejected. Freezes permissions at exchange time and requires re-exchange after any change.

**Explicit deny statements** — deferred. Additive grants are easier to reason about and sufficient for the initial use cases. Adding denies later is a breaking change to evaluation order, so if they are ever likely, decide before this ships.

**Per-request STS session policies to enforce prefix scope at the cloud** — considered as an alternative to proxy-side prefix enforcement. Attractive because the cloud enforces it, but it interacts badly with credential caching: see the cache-keying invariants in ADR-006, which require a session-policy fingerprint in the cache key in the same change. Prefer a bucket policy conditioned on the session subject where the cloud supports it.
