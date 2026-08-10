# ADR-010: Account-Owned Roles

**Status:** Proposed — not implemented
**Date:** 2026-08-09
**RFC:** RFC-001 §7
**Depends on:** ADR-004, ADR-009
**Blocks:** ADR-011

---

## Context

ADR-004 ships a single built-in Role, `_default`, with an unlimited ceiling and no subject conditions. Every credential the platform issues therefore carries the caller's full permissions. There is no way to obtain a narrower credential, and no way to say "this specific workload may write to this specific product".

The key design choice in this ADR is that **accounts own their Roles**. Rather than a fixed set of platform roles, each account (Individual or Organization) creates Roles that scope access to their resources, constrained to platform-registered IdPs. This mirrors how AWS IAM lets accounts configure their own trust policies and roles.

Neither the proxy nor `source.coop` has any Role model today — the only `role` in the codebase is organisation-membership role, an unrelated concept.

---

## Decision

### Role Identity and Schema

Roles belong to an account and are identified by URN: `sc::{account_id}::role/{role_name}`.

A Role defines two things: **who can assume it** (identity constraints) and **what its credentials can access** (permission statements).

```json
{
  "name": "github-publisher",
  "display_name": "GitHub CI Publisher",
  "max_session_duration": 3600,
  "identity_constraints": [
    {
      "idp": "github-actions",
      "audience": "https://data.source.coop",
      "claim_constraints": [
        {"claim": "repository", "operator": "equals", "value": "my-org/my-repo"},
        {"claim": "ref", "operator": "starts_with", "value": "refs/heads/"}
      ]
    }
  ],
  "permissions": [
    {"actions": ["read", "write"], "resources": ["sc::my-org::product/climate-data/*"]},
    {"actions": ["read"], "resources": ["sc::my-org::product/reference-data/*"]}
  ]
}
```

A Role acts as a **ceiling** on the account's existing permissions. Credentials issued for a Role can never exceed what the account itself has access to; the request-time intersection is specified in ADR-011.

### Identity Constraints

Each Role specifies one or more IdP bindings, each naming a platform IdP (ADR-009) and the claim constraints a presented JWT must satisfy.

| Operator | Behaviour | Example |
|---|---|---|
| `equals` | Exact string match | `repository` equals `my-org/my-repo` |
| `starts_with` | String prefix match | `ref` starts_with `refs/heads/` |

Rules:

- All claim values are coerced to strings before comparison. Array or object claims evaluate to false.
- Constraints within one IdP binding are ANDed; multiple bindings are ORed.
- **A missing claim evaluates to false (fail-closed).**
- Only top-level claims — no nested path traversal.

> [!NOTE]
> **Future extension: `glob` operator.** Wildcard matching (`*`, `?`) was considered and deferred. `equals` and `starts_with` cover constraining to a repo or a branch prefix. Adding `glob` later is backwards-compatible. Add it when users need mid-string wildcards (e.g. `refs/heads/release-*`).

### Permission Statements

```
*                                                    → all resources (unlimited ceiling)
sc::{account_id}::product/*                          → all of an account's products
sc::{account_id}::product/{product_name}             → entire product
sc::{account_id}::product/{product_name}/*           → entire product (equivalent)
sc::{account_id}::product/{product_name}/{prefix}/*  → prefix-scoped
sc::{account_id}::product/{product_name}/{key}       → single object
```

Rules:

- Patterns may reference **any** account's products. A Role can delegate access to products the owning account can reach, even when owned elsewhere; the request-time intersection enforces the real boundary.
- `*` as the whole resource means no ceiling — the account's actual permissions are the sole constraint.
- Actions are `read` (GetObject, HeadObject, ListObjects) and `write` (PutObject, DeleteObject, multipart). Finer actions can be added later as new values without breaking existing definitions.
- Statements are additive (allow-only). No explicit denies.

### The `_default` Role, Restated

Every account keeps a built-in `sc::{account_id}::role/_default`:

- Cannot be deleted
- Constrained to the `auth.source.coop` platform IdP only
- Permissions `{"actions": ["read", "write"], "resources": ["*"]}` — unlimited ceiling
- Owners may add claim constraints to its binding, but cannot change the IdP binding itself

This is the Role that ships today (ADR-004); this ADR generalises around it without changing its behaviour, so existing clients keep working unchanged.

### Validation at Creation

1. `name` matches `[a-z0-9][a-z0-9-]{0,62}`
2. Each IdP reference is a valid platform IdP ID
3. `max_session_duration` between 900 and 43200 seconds
4. At least one identity constraint
5. At least one permission statement
6. Maximum 10 IdP bindings per Role, 20 claim constraints per binding, 50 permission statements per Role

### Management API

```
POST   /api/v1/accounts/{account_id}/roles
GET    /api/v1/accounts/{account_id}/roles
GET    /api/v1/accounts/{account_id}/roles/{role_name}
PUT    /api/v1/accounts/{account_id}/roles/{role_name}
DELETE /api/v1/accounts/{account_id}/roles/{role_name}
```

Served by `source.coop`, which remains the schema owner (ADR-007). Only account owners and org admins may manage Roles.

### Proxy Changes

`StsCredentialRegistry::get_role` currently returns a hardcoded `_default` and carries a standing TODO to look Roles up via the API. It becomes a subject-scoped API lookup, cached like every other policy-store read (ADR-007), with `_default` synthesised rather than stored.

Roles are on the STS exchange path, not the S3 request path, so their cache TTL affects session establishment only — 30–60s is appropriate.

### The Organisation Subject Problem

ADR-005 records that the proxy signs policy-store calls with the caller's **Ory identity id**, and that `source.coop` resolves it through the identity index — organisation accounts are never the subject of a proxy-issued token.

Account-owned Roles break that assumption directly. A CI workflow assuming `sc::my-org::role/publisher` must resolve **my-org's** permissions, not those of the individual who configured it. So this ADR requires a paired change on the API side: accept an account id as `sub`, resolving either an individual or an organisation.

**This is the largest piece of work in this ADR and it is not proxy-side.** It should be designed before the Role schema is built, because it determines whether `sub` stays an Ory identity with the account carried separately, or becomes an account id as RFC-001 §11 assumed.

---

## Consequences

**Benefits**

- Accounts create their own access scopes with no operator involvement.
- Credentials can be narrower than the account — the least-privilege story the platform currently cannot tell.
- Claim constraints bind a credential to a specific workload ("only this repo, on this branch").
- Makes ADR-009 safe to enable: a CI token maps to a scoped Role rather than the user's whole account.
- Organisation-owned automation becomes representable.

**Costs / Risks**

- The largest single piece of unbuilt work in the RFC: schema, CRUD API, UI, proxy lookup, and the org-subject change.
- Role definitions are security-critical user input; the validation rules above are a floor, not a ceiling.
- A cached Role definition is a revocation lag on the exchange path — a tightened Role stays assumable for its TTL.
- The org-subject change touches the most sensitive code path in `source.coop`.
- Users can author Roles that grant nothing useful (a ceiling disjoint from their grants) and will need diagnostics that explain why, without leaking the trust policy.

---

## Alternatives Considered

**Fixed role set (`anonymous`, `authenticated_user`, `admin`)** — rejected. Cannot express "GitHub Actions from repo X may write dataset Y", which is the delegation use case.

**Keep only `_default` and scope credentials some other way** — considered, e.g. a `Scope` parameter on the exchange narrowing the credential without a stored Role. Simpler, and avoids CRUD entirely; rejected because a caller-supplied scope is self-asserted. The value of a Role is that its *trust policy* is authored by the resource owner, not the caller.

**Claim constraints on IdP registration rather than on the Role** — rejected. Different Roles for the same account need different constraints for the same IdP: one might require `refs/heads/main` while another allows any branch.

**Permission statements evaluated by the API instead of the proxy** — considered, and would avoid embedding a ceiling in the token. Rejected for the hot path: it would add a policy-evaluation round trip to every request, where the ceiling check is otherwise local (ADR-011).
