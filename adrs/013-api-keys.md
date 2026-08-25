# ADR-013: API Keys — Long-Lived Credentials for Service Accounts

**Status:** Proposed — not implemented
**Date:** 2026-04-01
**RFC:** RFC-001
**Depends on:** ADR-001, ADR-004, ADR-010, ADR-014, ADR-015

> [!NOTE]
> The `api-keys` endpoints that exist in `source.coop` today are the **legacy** admin-managed keys used by the pre-Workers proxy. They are unrelated to this design, and the current proxy has no code path that accepts them (ADR-001). This ADR proposes a replacement, not a formalisation of what is there. Note that six live HTTP routes in `source.coop` still read and write that table, so removing it needs a deprecation window rather than a straight delete — there is no remaining *consumer*, which is not the same as no code path.

---

## Context

ADR-004 defines inbound authentication via OIDC federation: callers present a JWT from a trusted identity provider and exchange it at `/.sts` for short-lived STS credentials. This works well for CI/CD platforms with ambient OIDC tokens (GitHub Actions, GitLab CI, etc.) and for interactive users who can complete a browser-based login via `auth.source.coop`.

However, a significant class of users has neither:

- Researchers running recurring batch jobs or cronjobs on university HPC clusters (SLURM, PBS, traditional login nodes)
- On-premises instruments or data loggers that push observations on a schedule
- Legacy ETL systems in environments without a supported OIDC issuer

These users have Source Cooperative accounts but operate in compute environments that do not issue OIDC tokens and cannot perform interactive browser authentication at runtime. ADR-001 and ADR-004 both identify this gap as future work.

A key is issued to a service account (ADR-015), not to a person. The workload gets an identity of its own that can be granted and revoked without touching the account of whoever set it up — which is what makes an unattended credential safe to hand out. This ADR covers only the credential; the principal, its ownership and its grants are ADR-015.

---

## Decision

### API Keys as Long-Lived JWTs

Source Cooperative issues API keys as long-lived JWTs signed by the data proxy's own signing key — the same key the proxy uses as an OIDC issuer for outbound storage authentication (ADR-006). The proxy already publishes its JWKS and `/.well-known/openid-configuration`; API key JWTs are verifiable against the same key material.

An API key JWT contains:

```json
{
  "iss": "https://data.source.coop",
  "sub": "<account_id>",
  "jti": "<unique_key_id>",
  "iat": 1711929600,
  "exp": 1743465600,
  "type": "api_key"
}
```

- `iss` is the proxy's own issuer URL, not `auth.source.coop` (which is Ory Network and outside Source Cooperative's control for token minting)
- `sub` identifies the principal the key belongs to — a service account (ADR-015), resolved through an identity binding (ADR-014) like any other subject. A key is issued to a service account, never to a person.
- `jti` is a unique key identifier used for revocation checks
- `exp` is **mandatory**, with a platform ceiling. See below.
- `type` distinguishes API key JWTs from other tokens the proxy may issue (e.g. outbound federation tokens)

### Keys Always Expire

An earlier draft of this ADR made `exp` optional, so a key would be "valid until explicitly revoked". Two things rule that out.

The practical one: an indefinite key outlives the person who created it. This ADR's own Costs section names the failure — a researcher leaves a university and their key keeps working until an administrator happens to revoke it.

The mechanical one: it cannot be delivered on the key material described below. A JWKS publishes the current key and one previous key, so a signature stops verifying two rotations after it was made, whatever the token says. A key with no `exp` would not be valid indefinitely; it would fail at an unpredictable point and look like an outage.

Keys carry an expiry chosen at creation, bounded by a platform maximum. Renewal is issue-new, deploy, revoke-old: rotation has no overlap window and cannot extend an existing key's expiry.

### Signing Key

API keys are signed with a dedicated key, published under its own `kid`, **not** the outbound federation key from ADR-006.

That key is a public contract: its issuer URL and thumbprint are registered in third-party cloud IAM configurations (ADR-012), so rotating it is a coordinated migration with external parties. Long-lived credentials must not depend on a key whose rotation schedule is set by someone else's cloud account, and a compromise of one should not force the other.

### Key Lifecycle

**Creation:**

Users create API keys via the Source Cooperative UI or CLI:

```
source keys create --service-account svc--ncar-cronjob --expires-in 90d
```

The system:
1. Generates a unique `jti`
2. Stores key metadata in the policy store: `jti`, service account id, label, created-at, expires-at
3. Mints and signs the JWT
4. Returns the raw JWT to the user — displayed once, never stored by the platform

**Revocation:**

Users revoke keys via the UI or CLI:

```
source keys revoke <key_id>
```

Revocation marks the key's `jti` as revoked in the policy store. The revocation takes effect within the `jti` validation cache TTL (see below).

**Management API:**

```
POST   /api/accounts/{account_id}/keys
GET    /api/accounts/{account_id}/keys
DELETE /api/accounts/{account_id}/keys/{key_id}
```

The `GET` endpoint returns key metadata (ID, label, created-at, expires-at, last-used-at) but never the JWT itself. Keys are managed by whoever can manage the service account's owner (ADR-015).

### STS Exchange

API key JWTs are exchanged at `/.sts` using the same flow as any other OIDC token (ADR-004) — `AssumeRoleWithWebIdentity` is an action parameter, not a path segment:

```
Action=AssumeRoleWithWebIdentity
&WebIdentityToken=<api_key_jwt>
&RoleArn=sc::my-org::role/publisher
&RoleSessionName=ncar-daily-sync
```

The STS exchange flow proceeds as defined in ADR-004 with one additional step:

1. Parse `RoleArn` → extract `account_id` and `role_name`
2. Load Role definition (cached)
3. Extract `iss` from JWT → matches `https://data.source.coop`
4. Verify JWT signature against the proxy's own JWKS
5. Verify `exp` (if present), `nbf`, `iat`
6. **Validate `jti` against the policy store** — confirm the key has not been revoked (cached, 30–60s TTL)
7. Evaluate claim constraints for the matched IdP binding
8. Validate `DurationSeconds` ≤ Role's `max_session_duration`
9. Generate credentials and return response

Step 6 is the only addition to the existing STS flow. For non-API-key tokens (those without `"type": "api_key"`), this step is skipped.

### Platform IdP Registration

The proxy's own issuer is registered as a platform IdP:

```json
{
  "id": "source-coop-api-key",
  "issuer_url": "https://data.source.coop",
  "display_name": "Source Cooperative API Key",
  "well_known_claims": ["type"],
  "audience_hint": "https://data.source.coop"
}
```

Roles that should be assumable via API key must include an identity constraint binding for this IdP:

```json
{
  "idp": "source-coop-api-key",
  "claim_constraints": [
    {"claim": "type", "operator": "equals", "value": "api_key"},
    {"claim": "sub", "operator": "equals", "value": "svc--ncar-cronjob"}
  ]
}
```

This reuses the Role and identity constraint model from ADR-010 without modification — and therefore depends on it, since no such model exists today. Account owners explicitly opt in to API key access per Role: a Role without a `source-coop-api-key` binding cannot be assumed with an API key.

The `sub` constraint names the service account, so a Role states which machine identity may assume it in exactly the way it states which GitHub repository may. There is one place a reviewer looks to see who can assume a Role.

### One Key, One Service Account

A key is bound to exactly one service account at creation and cannot be moved. Which Roles that service account may assume is recorded on the Roles themselves (ADR-010, ADR-015), so a key needs no Role binding of its own.

An earlier draft let a key optionally bind to a single Role, to limit the blast radius of a leak. That is now expressed by making a second service account with narrower grants, which limits the blast radius the same way and keeps one place where access is described.

### Caching and Revocation Latency

The `jti` validity check uses the same caching infrastructure as other policy store lookups (ADR-007): the Cloudflare Cache API, with a short TTL in line with the permission lookup.

This means revocation takes effect within roughly a minute. For the target use case (long-running cronjobs, batch pipelines), that latency is acceptable. If faster revocation is needed, rotating `SESSION_TOKEN_KEY` (ADR-001) invalidates all active STS sessions immediately — a more disruptive but available emergency response.

---

## Consequences

**Benefits**

- Covers the authentication gap for environments without OIDC or browser access
- No new auth path at the proxy layer — API key JWTs flow through the existing `/.sts` exchange, and no exchange endpoint is needed elsewhere
- **The key is the token file.** A stock AWS SDK reads the key from `AWS_WEB_IDENTITY_TOKEN_FILE` and calls `/.sts` itself, so unattended use needs no Source-specific code and no background process keeping a token fresh
- Reuses the Role and identity constraint model from ADR-010
- Revocation is explicit and auditable via `jti` lookup
- A key belongs to a service account, so it can be revoked without touching any person's access

**Costs / Risks**

- API key JWTs are bearer tokens — anyone with the raw JWT can use it. Users must treat them like passwords (store in environment variables or secret files, not in source control)
- The `jti` revocation check adds a policy store dependency to the STS exchange path for API key tokens. Cache misses add latency, and the check must fail closed: an unavailable policy store denies rather than skipping the check.
- Keys expire, so an unattended workload stops working on a known date. That is the intended trade against indefinite keys, but it fails silently — there is no notification channel, so expiry must be documented and surfaced in the UI.
- A second signing key to manage, publish and rotate.
- Revocation takes effect within the cache TTL, and credentials already issued live out their session regardless.

---

## Alternatives Considered

**Ory-issued long-lived tokens** — not feasible. `auth.source.coop` is Ory Network, which controls its own signing keys. Source Cooperative cannot mint arbitrary long-lived JWTs from Ory's issuer.

**OAuth2 client credentials grant** — considered. The client credentials grant authenticates an application, not a user — the resulting token's `sub` is the client ID, not a user identity. Mapping OAuth2 clients back to Source Cooperative accounts would require a bespoke service account system built on top of OAuth2.

**Ory personal access tokens** — investigated. Ory Network's PAT/API key concept (`ory_pat_`) is for project admin API access, not end-user authentication. User-scoped PATs are an [open feature request](https://github.com/ory/kratos/issues/1106) on Ory Kratos but not available.

**Opaque API keys with hash-based validation** — considered, and reconsidered. The platform generates a random secret, stores a hash, and validates by re-hashing.

The case for it is that this ADR's `jti` check already costs a policy-store call on the exchange path, so the token is not self-contained either way; once that call exists, a hash comparison is barely more work. The case against is what the opaque key cannot do: it is not a JWT, so it cannot be presented at `/.sts`, which means a separate exchange endpoint in `source.coop` to trade the key for a short-lived token, that endpoint's own rate limiting and error semantics, and a background process on the workload keeping the resulting token file fresh. The JWT is itself a valid `AWS_WEB_IDENTITY_TOKEN_FILE`, so none of that exists.

Rejected on that difference. Revocability does not distinguish the two: `jti` deny-listing revokes an individual key with the same lookup and the same lag as a hashed-secret check.

**Managed key storage (Ory Talos)** — rejected. A hosted key service supplies hashing, forced expiry, rotation and a self-revoke endpoint. Since keys here are JWTs, there is no secret to store and hash, so what remains is metadata this platform already keeps. It would also add a vendor on the credential path with an undisclosed per-project key ceiling and an unstated verification cache TTL, which would set the real revocation window. Revisit only if opaque keys are adopted after all.

**Long-lived static credentials in the proxy's credential registry** — rejected. `StsCredentialRegistry::get_credential` is a stub returning nothing, and upstream models a scoped, expiring, revocable stored credential, so this looks like the shortest path. It is not: S3 request signing is symmetric, so the proxy would have to recover each key's plaintext secret to verify a signature. That reintroduces a store of usable secrets, which is what ADR-001 and this ADR both exist to avoid.

**Long-lived Ory refresh tokens** — considered as a near-term workaround. The user performs a one-time `source login` (device flow) and stores the refresh token. Cronjobs silently refresh access tokens. This works without new infrastructure but refresh tokens expire eventually, causing silent failures in unattended workflows. Suitable as an interim measure but not a durable solution for indefinitely recurring workloads.
