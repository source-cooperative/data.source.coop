# ADR-005: Authorization Model — Role Ceiling with Dynamic Account Permission Resolution

**Date:** 2026-03-14
**RFC:** RFC-001 §8
**Depends on:** ADR-001, ADR-004
**Language:** ASD-STE100 Simplified Technical English

---

## Context

ADR-001 specifies that a session token is a stateless JWT. ADR-004 introduces the account-owned Role, which contains permission statements. Those statements are a ceiling on the access of the credentials of the Role. This ADR specifies how the proxy resolves the permissions at request time.

Two properties control the design.

1. **The Role is a ceiling, and the account permissions are the grants.** The permission statements of the Role are in the SessionToken from the exchange. They answer this question: what is the maximum access of these credentials? The lookup of the account permissions answers a different question: what can this account actually access? The proxy applies the intersection of the two. Thus a Role can decrease the access, but it can never increase it.

2. **Account permissions are dynamic.** A user can become a member of an organisation or get a grant on a new dataset. That user must see the change immediately. The proxy gets the account permissions from the policy store for each request and does not freeze them in the token. Thus a change becomes effective in one cache TTL.

AWS IAM operates in the same way. The session token asserts the role membership with an embedded permission boundary, and AWS evaluates the current policies of the role at each API call.

---

## Decision

### Identity Model

The SessionToken (refer to ADR-001) contains these fields for the authorization:

- `account_id` — the account whose permissions are the base grants
- `role_name` — the identifier of the Role, for logs and for the ceiling lookup
- `permissions` — the permission statements of the Role from the exchange (the ceiling)
- `assumed_by` — the original IdP subject, for audit and not for authorization
- `exp` — the expiry of the token. The proxy checks it before it evaluates any policy.

### How Roles Replace the Fixed Role Set

The previous design had three fixed roles: `anonymous`, `authenticated_user`, and `admin`. Roles that the users define (refer to ADR-004) replace these three. The Role configuration gives the equivalent behaviour.

**Anonymous access** uses no Role. The proxy treats a request with no credentials as anonymous. An anonymous caller can read only public products. The proxy does no Role lookup and no account permission lookup.

**Authenticated user access** uses the built-in `_default` Role, which has an unlimited ceiling (`"resources": ["*"]`). Thus the actual permissions of the account are the only limit. This is equivalent to the previous `authenticated_user` role.

**Admin access** comes from the account permissions in the policy store, and not from a special role type. An account with admin grants has larger permissions. The Role ceiling does not decrease them when the caller uses the `_default` Role with `*` resources.

**Scoped access** is the new capability. A Role with specific permission statements, for example read-only on one product, makes a small ceiling. The account can have large permissions, but the credentials can access only what the Role permits.

### Authorization of Each Request

The authorization has six steps. Each step can permit or refuse the request immediately, to keep the number of lookups small.

**Step 1 — Identify the caller**

- **No credentials** → the caller is anonymous. The proxy permits only read actions on public products.
- **STS credentials** (the `SCSTS` prefix) → the proxy calculates the SecretAccessKey with HMAC, checks the SigV4 signature, and decodes the SessionToken JWT.

> [!NOTE]
> **Future extension: Permanent API keys.** The first implementation supports only STS credentials and anonymous access. Long-lived API keys can become necessary for workflows that have neither workload identity federation nor interactive authentication with `auth.source.coop`. Examples are on-premises instruments, legacy ETL systems, and environments with no OIDC support. We will not add a second authorization path to the proxy. Instead, a caller exchanges an API key for temporary STS credentials at the `/.sts` endpoint, in the same way as an OIDC token. Thus the authorization at request time stays uniform: the proxy accepts only short-lived STS credentials on an S3 API call.

**Step 2 — Role action check (in memory, no lookup)**

For an anonymous caller, the proxy permits only read actions: `GetObject`, `HeadObject`, `ListObjects`, and `ListBuckets`. It refuses a write immediately.

For an STS caller, the proxy looks in the `permissions` array of the SessionToken. If no permission statement agrees with the requested action (read or write) and the requested resource, the proxy refuses the request immediately. This check uses only data that is already in the token. It makes no network call.

**Step 3 — Resource resolution**

The proxy maps the S3 request to a Source Cooperative resource:
- The bucket name becomes `account_id/product_name`.
- The object key becomes the path in the product.

**Step 4 — Early exit for public resources (cached, 60–300 s TTL)**

For a read request, the proxy permits the request immediately if the product is public (`data_mode: open`). It does no more lookups. This is the fast path for most of the traffic, which is reads of public open data.

**Step 5 — Account permission lookup (cached, 30–60 s TTL)**

For a resource that is not public, or for a write operation, the proxy does these steps:
1. Get the account permissions from the Source Cooperative API. The `account_id` in the SessionToken identifies the account.
2. Calculate `(Role ceiling permissions from token) ∩ (account's actual permissions from API)`.
3. If the intersection contains the requested action on the requested resource, permit the request.
4. If it does not, refuse the request.

The proxy does not evaluate the organisation membership or the inheritance of permissions. The API does this internally. When a user is a member of an organisation, the API includes the inherited permissions in the grants of that account. The proxy uses the response of the API as the authoritative permission set of the account.

### Proxy-to-API Authentication

The proxy authenticates each policy store lookup as the account whose permissions it needs. Thus the API needs no separate "service account" code path. The same authorization logic serves the frontend and the proxy.

#### How `account_id` Comes into a Proxy-to-API Request

The proxy uses the `account_id` as the `sub` claim. That `account_id` comes from the Role and moves through the credential chain in these steps:

1. **Role lookup.** A caller sends an OIDC token to `/.sts` and asks for a specific Role. An account owns that Role. That account can be a **user account** or an **organisation account**. The ID of the owner account becomes the `account_id`.

2. **SessionToken issue.** The proxy makes a SessionToken JWT. That token contains the `account_id`, the `role_name`, the `permissions` (the ceiling of the Role), and the `assumed_by` field (the original IdP subject of the caller).

3. **STS credential issue.** The proxy puts the SessionToken into the STS credentials and returns them to the caller. The `AccessKeyId` contains the `account_id` and gives the signing key. The SessionToken JWT is the `SessionToken` value.

4. **Decode at request time.** The proxy receives an S3 request with a signature from these credentials. It checks the SigV4 signature, decodes the SessionToken, and reads the `account_id`.

5. **Policy store lookup.** The proxy makes a new short-lived JWT with `sub: account_id`. It sends that JWT to the Source Cooperative API and gets the permissions of that account.

#### The `sub` Claim Is an Account and Not Always a User

The `sub` claim in the proxy-to-API JWT is an **account ID**. That account can be a user or an organisation:

- **User account.** A user authenticates with the frontend or with an IdP and assumes the `_default` Role of that user. Then `sub` is the account ID of the user. The API returns the permissions of that user. The response is the same as for a direct request from the frontend.

- **Organisation account.** A CI workflow authenticates with GitHub OIDC and assumes a Role that `my-org` owns. Then `sub` is `my-org`, the account ID of the organisation. The API returns the full permissions of `my-org`. These permissions include the grants on products of other accounts that `my-org` can access. The proxy evaluates the Role ceiling locally, and that ceiling limits what the CI workflow can do.

The API does not distinguish between these two cases. In both cases, the API receives an account ID and returns the permissions of that account. The proxy applies the intersection with the Role ceiling locally.

#### JWT Claims

For each policy store request, the proxy makes a short-lived JWT and signs it with its private key:

| Claim | Value | Purpose |
|-------|-------|---------|
| `sub` | `account_id` from the SessionToken | Tells the API whose permissions to return. Can be a user ID or an org ID. |
| `iss` | The proxy's OIDC issuer URL | The API checks the signature against the proxy's `/.well-known/jwks.json`. |
| `aud` | The API's base URL | Limits the token to the policy store API. |
| `role` | `role_name` from the SessionToken | Information for the audit log. Does not change the API response. |
| `assumed_by` | `assumed_by` from the SessionToken | The original IdP subject, for the audit trail. |
| `exp` | Short-lived (≤ 60s) | Limits the replay window. |

#### Trust Model

The API trusts the proxy to assert any `sub` value. This trust comes from the JWT signature. The API checks that signature against the OIDC discovery document that the proxy publishes. Only the proxy holds the signing key. An AWS service that assumes an IAM role uses the same model: the service is trusted to operate for the principal.

#### Why `sub` Is the `account_id` and Not the `assumed_by` Value

The permission model of the API is account-centric. Source Cooperative gives each grant to an account, which is a user or an organisation. It does not give a grant to an external IdP subject. A GitHub Actions workflow can assume a Role of an organisation. Then the relevant permissions are the permissions of that organisation, and not the permissions of the workflow. The proxy evaluates the Role ceiling locally, and that ceiling limits what the workflow can do. The `assumed_by` claim keeps the attribution for the audit and has no effect on the authorization.

**Step 6 — Prefix enforcement**

A permission statement of a Role can contain a prefix constraint, for example `sc::my-org::product/my-dataset/uploads/*`. The proxy then makes sure that the object key is in that prefix. This enforcement is a part of Step 2 and Step 5: the proxy evaluates the prefix when it compares the resource pattern.

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

### Behaviour of Each Operation

**Operations on one resource (`GetObject`, `PutObject`, `HeadObject`, `DeleteObject`)**
After the Role ceiling check and the early exit for public products, the proxy does one lookup: has the account a grant for this product? If that grant contains prefix limits, the proxy applies them to the requested object key.

**`ListBuckets`**
The proxy makes this response fully from the policy store and does not call the upstream storage:
1. For an anonymous caller, return the products with `public = true`.
2. For an STS caller with the `_default` Role (an unlimited ceiling), return all products for which the account has a grant.
3. For an STS caller with a scoped Role, return only the products that are in the permission statements of the Role and in the grants of the account.

**`ListObjects` (in one product)**
The proxy does the Role ceiling check, the early exit for public products, and the account permission lookup. If the permission statement of the Role contains a key prefix limit, the proxy sends that prefix as a filter to the upstream `ListObjects` call.

### How the Proxy Matches a Permission Statement

To find if a request agrees with a permission statement of a Role, the proxy makes two checks:

1. **Action match:** Does the `actions` array of the statement contain the requested action class, `read` or `write`? ADR-004 specifies the action classes.
2. **Resource match:** Does the `resources` array of the statement contain a pattern that agrees with the requested resource?
   - `*` agrees with all resources.
   - `sc::{account}::product/{name}` and `sc::{account}::product/{name}/*` agree with the full product.
   - `sc::{account}::product/{name}/{prefix}/*` agrees with the objects below the prefix.
   - `sc::{account}::product/{name}/{key}` agrees with one object.

If one statement agrees with the action and with the resource, the Role permits the request. The account permission lookup then finds if the account actually has that access.

### Cache Strategy

The proxy caches each lookup of the policy store in its own process (one cache for each isolate).

| Lookup | Cache Key | TTL |
|---|---|---|
| Product public flag | `product_id` | 60–300s |
| Account permission for product | `(account_id, product_id)` | 30–60s |
| Account's full product list (`ListBuckets`) | `account_id` | 5–10s |

The short TTL of the full product list makes each change to the account permissions visible in a few seconds. These changes include a new grant and a new organisation membership.

In Workers, the cache belongs to one isolate and the edge nodes do not share it. Workers KV is available as a shared tier if it becomes necessary.

### Access Logs

Each S3 request with STS credentials writes a structured log entry:

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

This gives a full audit trail. It shows the account, the Role, the original identity, and the resource.

### S3 Error Responses

When the authorization refuses a request, the proxy returns a standard S3 error response:

```xml
<Error>
  <Code>AccessDenied</Code>
  <Message>Access Denied</Message>
  <RequestId>...</RequestId>
</Error>
```

The HTTP status is `403 Forbidden`. The error body does not show the cause of the refusal. The cause can be the Role ceiling, an account permission that does not exist, or a product that does not exist. Thus the response shows nothing about the existence of a resource.

For `ListBuckets` and `ListObjects`, the proxy removes the resources from the result and returns no error. Thus the caller sees only the resources that the caller can access.

---

## Consequences

**Benefits**

- The proxy evaluates the Role ceiling locally from the SessionToken. Thus the first authorization check needs no network call.
- The account permissions are always current. A user does not do a new exchange after the user makes a dataset or becomes a member of an organisation.
- Most of the traffic is reads of public datasets. These reads need no account lookup.
- The format of a permission statement is concrete: the actions are `read` and `write`, and a resource is a URN pattern with an optional prefix.
- The model supports delegation. A Role can refer to a product of a different account, if the account of the Role can access that product.
- The audit logs contain the account identity and the original IdP subject. Thus the attribution is possible, although the credentials operate as the account.
- Anonymous access stays easy. There is no STS exchange and there are no credentials. The caller uses `--no-sign-request`.

**Costs and Risks**

- Each authenticated request on a resource that is not public needs an account permission lookup in the policy store. The cache decreases this load.
- The policy store is on the hot path. If it is not available, the latency of a cache miss increases.
- In Workers, the cache belongs to one isolate. The edge nodes do not share it, thus a cold isolate always causes a cache miss.
- The permission model is additive and permits only. This iteration has no explicit denials. To give access to all products but one, you must make one grant for each of the other products.
- For a scoped Role, the `ListBuckets` response needs the intersection of the resource patterns of the Role and the grants of the account. This is more complex than a response with the full product list of the account.

---

## Alternatives Considered

**Put the full permissions in the session token** — rejected. This freezes the permissions at the exchange. A user must then do a new exchange to see a permission change. This is not acceptable on a platform where users make datasets and become members of organisations frequently. The hybrid approach gives both properties: the Role ceiling is in the token and the check is local, and a permission change becomes visible almost immediately.

**A fixed role set (`anonymous`, `authenticated_user`, `admin`)** — superseded by the Roles that users define. The `_default` Role with an unlimited ceiling gives the same result as `authenticated_user`. The account grants give the admin access, and no special role type is necessary. A scoped Role gives a new capability that the fixed set cannot express.

**A central permission cache (Redis or Workers KV as the primary tier)** — considered. All isolates and containers then share the cache. We rejected it as the primary tier, because it adds a network hop to each cache read. An in-process cache with Workers KV as a second tier is better.

**Explicit denials in the grants** — deferred. Additive grants are more simple to understand and are sufficient for the first use cases. We can add explicit denials later if the access control model needs them.

**A separate principal identity for delegated access** — considered. The STS credentials then identify a separate principal, for example "github-actions through account/role", and do not operate as the account. We rejected it. It makes the permission model more complex, because each delegated principal then needs its own grants, and it gives no clear benefit. The Role ceiling already limits the credentials. The `assumed_by` field in the SessionToken keeps the audit trail separate and needs no second authorization path.
