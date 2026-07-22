# ADR-008: API Keys for Environments Without OIDC

**Date:** 2026-04-01
**RFC:** RFC-001
**Depends on:** ADR-001, ADR-004, ADR-006

---

## Context

ADR-004 defines inbound authentication via OIDC federation: callers present a JWT from a trusted identity provider and exchange it at `/.sts` for short-lived STS credentials. This works well for CI/CD platforms with ambient OIDC tokens (GitHub Actions, GitLab CI, etc.) and for interactive users who can complete a browser-based login via `auth.source.coop`.

However, a significant class of users has neither:

- Researchers running recurring batch jobs or cronjobs on university HPC clusters (SLURM, PBS, traditional login nodes)
- On-premises instruments or data loggers that push observations on a schedule
- Legacy ETL systems in environments without a supported OIDC issuer

These users have Source Cooperative accounts but operate in compute environments that do not issue OIDC tokens and cannot perform interactive browser authentication at runtime. ADR-001 and ADR-004 both identify this gap as future work.

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
- `sub` identifies the Source Cooperative account that owns the key
- `jti` is a unique key identifier used for revocation checks
- `exp` is optional — keys without an expiry are valid until explicitly revoked
- `type` distinguishes API key JWTs from other tokens the proxy may issue (e.g. outbound federation tokens)

### Key Lifecycle

**Creation:**

Users create API keys via the Source Cooperative UI or CLI:

```
source keys create --label "ncar-cronjob" --role sc::my-org::role/publisher
```

The system:
1. Generates a unique `jti`
2. Stores key metadata in the policy store: `jti`, account ID, label, bound Role (optional), created-at, expires-at (nullable)
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

The `GET` endpoint returns key metadata (ID, label, created-at, expires-at, last-used-at) but never the JWT itself. Only account owners and org admins can manage keys.

### STS Exchange

API key JWTs are exchanged at `/.sts/assume-role-with-web-identity` using the same flow as any other OIDC token (ADR-004):

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
    {"claim": "type", "operator": "equals", "value": "api_key"}
  ]
}
```

This reuses the existing Role and identity constraint model from ADR-004 without modification. Account owners explicitly opt in to API key access per Role — a Role without a `source-coop-api-key` binding cannot be assumed with an API key.

### Role Binding

API keys can optionally be bound to a specific Role at creation time. A bound key can only be used to assume that Role. An unbound key can assume any Role the account owns that has a `source-coop-api-key` identity constraint.

Bound keys reduce blast radius: if leaked, the key can only access what that specific Role permits. For high-value automated workflows, bound keys are recommended.

### Caching and Revocation Latency

The `jti` validity check uses the same caching infrastructure as other policy store lookups (ADR-007):

- In-process cache with 30–60s TTL
- Workers KV as a shared cache tier

This means revocation takes effect within 30–60 seconds. For the target use case (long-running cronjobs, batch pipelines), this latency is acceptable. If faster revocation is needed, the HMAC server secret rotation mechanism from ADR-001 invalidates all active STS sessions immediately — a more disruptive but available emergency response.

---

## Consequences

**Benefits**

- Covers the authentication gap for environments without OIDC or browser access
- No new auth path at the proxy layer — API key JWTs flow through the existing `/.sts` exchange
- Reuses the proxy's existing OIDC issuer infrastructure (signing key, JWKS) from ADR-006
- Reuses the existing Role and identity constraint model from ADR-004
- Revocation is explicit and auditable via `jti` lookup
- Optional Role binding limits blast radius of leaked keys

**Costs / Risks**

- API key JWTs are bearer tokens — anyone with the raw JWT can use it. Users must treat them like passwords (store in environment variables or secret files, not in source control)
- The `jti` revocation check adds a policy store dependency to the STS exchange path for API key tokens. Cache misses add latency.
- Keys without expiry are valid indefinitely until revoked. If a user loses access to the management UI (e.g. leaves a university), orphaned keys persist unless an org admin revokes them.
- The proxy's signing key is now used for two purposes: outbound federation tokens (ADR-006) and API key JWTs. A signing key compromise affects both. Key rotation must account for both uses.

---

## Alternatives Considered

**Ory-issued long-lived tokens** — not feasible. `auth.source.coop` is Ory Network, which controls its own signing keys. Source Cooperative cannot mint arbitrary long-lived JWTs from Ory's issuer.

**OAuth2 client credentials grant** — considered. The client credentials grant authenticates an application, not a user — the resulting token's `sub` is the client ID, not a user identity. Mapping OAuth2 clients back to Source Cooperative accounts would require a bespoke service account system built on top of OAuth2.

**Ory personal access tokens** — investigated. Ory Network's PAT/API key concept (`ory_pat_`) is for project admin API access, not end-user authentication. User-scoped PATs are an [open feature request](https://github.com/ory/kratos/issues/1106) on Ory Kratos but not available.

**Opaque API keys with hash-based validation** — considered. The platform generates a random secret, stores a hash, and validates by re-hashing. This works but requires a dedicated validation endpoint or a new auth path at `/.sts`. The JWT approach avoids this by making API keys indistinguishable from other OIDC tokens at the STS layer — no new endpoint, no new validation logic beyond the `jti` check.

**Long-lived Ory refresh tokens** — considered as a near-term workaround. The user performs a one-time `source login` (device flow) and stores the refresh token. Cronjobs silently refresh access tokens. This works without new infrastructure but refresh tokens expire eventually, causing silent failures in unattended workflows. Suitable as an interim measure but not a durable solution for indefinitely recurring workloads.
