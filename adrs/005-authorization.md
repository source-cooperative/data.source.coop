# ADR-005: Authorization Model — Role Ceiling with Dynamic Account Permission Resolution

**Status:** Proposed
**Date:** 2026-03-14
**RFC:** RFC-001 §8
**Depends on:** ADR-001, ADR-004

---

## Context

ADR-001 establishes that session tokens are stateless JWTs. ADR-004 introduces account-owned Roles with embedded permission statements that define a ceiling on what the Role's credentials can access. This ADR defines how permissions are resolved at request time.

Two properties drive the design:

1. **The Role is a ceiling; account permissions are the grants.** The Role's permission statements (embedded in the SessionToken at exchange time) answer "what is the maximum scope of access for these credentials?" The per-account permission lookup answers "what can this account actually access?" The proxy enforces the intersection. A Role can narrow access but never widen it beyond what the account has.

2. **Account permissions are dynamic.** A user who joins an organisation or receives a grant on a new dataset should see that change reflected immediately. Because account permissions are resolved per-request from the policy store (not frozen in the token), changes propagate within the cache TTL.

This mirrors AWS IAM: the session token asserts role membership with embedded permission boundaries, and the role's current policies are evaluated live on each API call.

---

## Decision

### Identity Model

The SessionToken (see ADR-001) carries these fields relevant to authorization:

- `account_id` — the account whose permissions form the base grants
- `role_name` — identifies the Role (for logging and ceiling lookup)
- `permissions` — the Role's permission statements, embedded at exchange time (the ceiling)
- `assumed_by` — the original IdP subject (for audit, not authorization)
- `exp` — token expiry; checked before any policy evaluation

### How Roles Replace the Fixed Role Set

The previous design used three fixed roles: `anonymous`, `authenticated_user`, and `admin`. These are replaced by user-defined Roles (see ADR-004). The equivalent behaviour is achieved through Role configuration:

**Anonymous access** does not use a Role at all. Requests without credentials are treated as anonymous. Anonymous callers can only read public products — no Role lookup, no account permission lookup.

**Authenticated user access** uses the built-in `_default` Role, which has an unlimited ceiling (`"resources": ["*"]`). The account's actual permissions are the sole constraint. This is equivalent to the previous `authenticated_user` role.

**Admin access** is determined by account permissions in the policy store, not by a special role type. An account with admin-level grants simply has broader permissions that the Role ceiling does not restrict (when using the `_default` Role with `*` resources).

**Scoped access** is the new capability. A Role with specific permission statements (e.g., read-only on one product) creates a narrow ceiling. Even if the account has broad permissions, the credentials can only access what the Role allows.

### Per-Request Authorization

Authorization proceeds in steps, with early exits to minimise lookups:

**Step 1 — Identify the caller**

- **No credentials** → anonymous. Only read actions on public products are permitted.
- **STS credentials** (`SCSTS` prefix) → derive SecretAccessKey via HMAC, verify SigV4, decode SessionToken JWT.

> [!NOTE]
> **Future extension: Permanent API keys.** The initial implementation supports only STS credentials and anonymous access. Long-lived API keys may be needed in the future for workflows where neither workload identity federation nor interactive authentication via `auth.source.coop` is feasible — for example, on-premises instruments, legacy ETL systems, or environments without OIDC support. Rather than adding a second authorization path to the proxy, API keys would be exchanged for temporary STS credentials at the `/.sts` endpoint — the same way OIDC tokens are. The proxy's request-time authorization remains uniform: only short-lived STS credentials are accepted on S3 API calls.

**Step 2 — Role action check (in-memory, no lookup)**

For anonymous callers, only read actions are permitted (`GetObject`, `HeadObject`, `ListObjects`, `ListBuckets`). Deny writes immediately.

For STS callers, check the SessionToken's embedded `permissions` array. If the requested action (read or write) on the requested resource does not match any permission statement, deny immediately. This is a local check against data already in the token — no network call.

**Step 3 — Resource resolution**

Map the S3 request to a Source Cooperative resource:
- Bucket name → `account_id/product_name`
- Object key → path within the product

**Step 4 — Public resource early exit (cached, 60–300s TTL)**

For read requests: if the product is public (`data_mode: open`), permit immediately. No further lookups. This is the fast path for the majority of traffic — public open data reads.

**Step 5 — Account permission lookup (cached, 30–60s TTL)**

For non-public resources or write operations:
1. Fetch the account's permissions from the Source Cooperative API (the account referenced in the SessionToken's `account_id`)
2. Compute: `(Role ceiling permissions from token) ∩ (account's actual permissions from API)`
3. If the intersection includes the requested action on the requested resource → permit
4. Otherwise → deny

The proxy does not evaluate org membership or permission inheritance logic — the API resolves these internally. When a user belongs to an organisation, the API includes permissions inherited through that membership in the account's resolved grants. The proxy treats the API response as the authoritative set of permissions for the account.

**Step 6 — Prefix enforcement**

If the Role's permission statement includes a prefix constraint (e.g., `sc::my-org::product/my-dataset/uploads/*`), verify the object key falls within that prefix. This enforcement is part of Step 2 and Step 5 — the prefix is evaluated when matching the resource pattern.

### Authorization Truth Table

| Caller | Resource | Account has access? | Role permits? | Result |
|--------|----------|-------------------|--------------|--------|
| Anonymous | Public product | N/A | N/A | **Allow** (read only) |
| Anonymous | Private product | N/A | N/A | **Deny** |
| STS | Public product, read | N/A | Yes | **Allow** |
| STS | Public product, write | Yes | Yes | **Allow** |
| STS | Private product | Yes | Yes | **Allow** |
| STS | Private product | Yes | No (ceiling) | **Deny** |
| STS | Private product | No | Yes | **Deny** |

### Operation-Specific Behaviour

**Single-resource operations (`GetObject`, `PutObject`, `HeadObject`, `DeleteObject`)**
After the Role ceiling check and public early exit, a point lookup: does the account have an access grant for this product? If the grant includes prefix restrictions, those are enforced against the requested object key.

**`ListBuckets`**
The proxy constructs this response entirely from the policy store — the upstream is never called:
1. Anonymous: return products with `public = true`
2. STS with `_default` Role (unlimited ceiling): return all products the account has grants for
3. STS with scoped Role: return only products that appear in both the Role's permission statements and the account's grants

**`ListObjects` (within a product)**
After the Role ceiling check, public early exit, and account permission lookup: if the Role's permission statement includes a key prefix restriction, pass it as a filter to the upstream `ListObjects` call.

### Permission Statement Matching

When evaluating whether a request matches a Role's permission statements, the proxy checks:

1. **Action match:** Does the statement's `actions` array include the requested action class (`read` or `write`)? See ADR-004 for the definition of action classes.
2. **Resource match:** Does the statement's `resources` array contain a pattern that matches the requested resource?
   - `*` matches everything
   - `sc::{account}::product/{name}` or `sc::{account}::product/{name}/*` matches the entire product
   - `sc::{account}::product/{name}/{prefix}/*` matches objects under the prefix
   - `sc::{account}::product/{name}/{key}` matches a single object

If any statement matches both action and resource, the Role permits the request. The account permission lookup then determines whether the account actually has the underlying access.

### Cache Strategy

All policy store lookups are cached in-process (per-isolate):

| Lookup | Cache Key | TTL |
|---|---|---|
| Product public flag | `product_id` | 60–300s |
| Account permission for product | `(account_id, product_id)` | 30–60s |
| Account's full product list (`ListBuckets`) | `account_id` | 5–10s |

The short TTL on the full product list ensures that account permission changes (new grants, org membership) are reflected within seconds.

For Workers, cache is per-isolate and not shared across edge nodes. Workers KV is available as a shared tier if needed.

### Access Logging

Every S3 request with STS credentials emits a structured log entry:

```json
{
  "event": "s3_request",
  "timestamp": "...",
  "account_id": "my-org",
  "role_name": "github-publisher",
  "session_name": "my-ci-job-42",
  "assumed_by": "repo:my-org/my-repo:ref:refs/heads/main",
  "action": "PutObject",
  "resource": "sc::my-org::product/climate-data/2025/data.parquet",
  "result": "allow",
  "client_ip": "..."
}
```

This provides full auditability: which account, which Role, which original identity, and what they accessed.

### S3 Error Responses

When authorization denies a request, the proxy returns a standard S3 error response:

```xml
<Error>
  <Code>AccessDenied</Code>
  <Message>Access Denied</Message>
  <RequestId>...</RequestId>
</Error>
```

HTTP status is `403 Forbidden`. The error body does not reveal whether the denial was due to the Role ceiling, missing account permissions, or a non-existent product — this prevents information leakage about resource existence.

For `ListBuckets` and `ListObjects`, the proxy filters results silently rather than returning errors. The caller sees only the resources they have access to.

---

## Consequences

**Benefits**

- The Role ceiling is evaluated locally from the SessionToken — no network call required for the first authorization check.
- Account permissions reflect current state — no re-exchange required after creating a new dataset or joining an organisation.
- The majority of traffic (public dataset reads) resolves with no account-specific lookup.
- The permission statement format is concrete and resolved: actions are `read`/`write`, resources use a URN pattern with optional prefix scoping.
- The model supports delegation: a Role can reference products owned by other accounts that the Role's account has access to.
- Audit logs capture both the account identity and the original IdP subject, enabling attribution even though credentials act as the account.
- Anonymous access remains frictionless — no STS exchange, no credentials, just `--no-sign-request`.

**Costs / Risks**

- Every non-public authenticated request requires an account permission lookup from the policy store (mitigated by caching).
- The policy store is on the hot path — its availability affects request latency for cache misses.
- Per-isolate caching in Workers means cache is not shared across edge nodes (cold isolate = cache miss).
- The permission model is additive (allow-only). Explicit denies are not supported in this iteration. If the access control model requires "grant access to everything except X," it must be expressed as individual grants for everything except X.
- The `ListBuckets` response for scoped Roles requires intersecting the Role's resource patterns with the account's grants, which is more complex than simply returning the account's full product list.

---

## Alternatives Considered

**Encode full permissions in the session token** — rejected. Freezes permissions at exchange time. Users would need to re-exchange tokens to see permission changes. Unacceptable for a platform where users create datasets and join organisations dynamically. The hybrid approach (Role ceiling in token, account permissions dynamic) provides the best of both: the ceiling check is local, and permission changes propagate in near real-time.

**Fixed role set (`anonymous`, `authenticated_user`, `admin`)** — superseded by user-defined Roles. The `_default` Role with unlimited ceiling achieves the same effect as `authenticated_user`. Admin access is determined by account grants, not a special role type. Scoped Roles provide new capability that the fixed set could not express.

**Centralised permission cache (Redis / Workers KV as primary)** — considered. Would share cache across isolates and containers. Rejected as the primary tier: adds a network hop to every cache read. Per-isolate caching with optional Workers KV as a secondary tier is preferred.

**Explicit deny support in grants** — deferred. Additive grants are simpler to reason about and sufficient for the initial use cases. Explicit denies can be added later if the access control model requires it.

**Separate principal identity for delegated access** — considered. STS credentials would represent a distinct principal (e.g., "github-actions via account/role") rather than acting as the account. Rejected: adds complexity to the permission model (need grants for delegated principals) without clear benefit. The Role ceiling already constrains what the credentials can do. The `assumed_by` field in the SessionToken provides audit trail separation without requiring a separate authorization path.
