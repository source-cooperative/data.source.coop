# ADR-008: API Keys for Environments Without OIDC

**Date:** 2026-04-01
**RFC:** RFC-001
**Depends on:** ADR-001, ADR-004, ADR-006
**Language:** ASD-STE100 Simplified Technical English

---

## Context

ADR-004 specifies the inbound authentication with OIDC federation. A caller sends a JWT from a trusted identity provider to `/.sts` and gets short-lived STS credentials in exchange. This operates correctly for a CI/CD platform with an ambient OIDC token, such as GitHub Actions or GitLab CI. It also operates correctly for an interactive user who can do a browser login at `auth.source.coop`.

But a large group of users has neither of these:

- Researchers who run recurring batch jobs or cronjobs on a university HPC cluster (SLURM, PBS, or a traditional login node)
- On-premises instruments or data loggers that send observations at a given interval
- Legacy ETL systems in an environment with no supported OIDC issuer

These users have Source Cooperative accounts. But their compute environments issue no OIDC token, and they cannot do a browser authentication at runtime. ADR-001 and ADR-004 both identify this gap as future work.

---

## Decision

### An API Key Is a Long-Lived JWT

Source Cooperative issues each API key as a long-lived JWT. The data proxy signs the key with its own signing key. This is the same key that the proxy uses as an OIDC issuer for the outbound storage authentication (ADR-006). The proxy already publishes its JWKS and its `/.well-known/openid-configuration` document. Thus the same key material checks an API key JWT.

An API key JWT contains these claims:

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

- `iss` is the issuer URL of the proxy. It is not `auth.source.coop`, because that issuer is Ory Network. Source Cooperative cannot make a token from that issuer.
- `sub` identifies the Source Cooperative account that owns the key.
- `jti` is a unique key identifier. The proxy uses it to check for a revocation.
- `exp` is optional. A key with no expiry stays valid until a person revokes it.
- `type` distinguishes an API key JWT from the other tokens of the proxy, such as an outbound federation token.

### Key Lifecycle

**Creation:**

A user makes an API key in the Source Cooperative UI or CLI:

```
source keys create --label "ncar-cronjob" --role sc::my-org::role/publisher
```

The system then does these steps:
1. It makes a unique `jti`.
2. It writes the key metadata to the policy store: the `jti`, the account ID, the label, the bound Role (optional), the creation time, and the expiry time (which can be empty).
3. It makes the JWT and signs it.
4. It returns the raw JWT to the user. The UI shows the JWT one time, and the platform does not keep it.

**Revocation:**

A user revokes a key in the UI or CLI:

```
source keys revoke <key_id>
```

The system then marks the `jti` of that key as revoked in the policy store. The revocation becomes effective after the TTL of the `jti` cache (refer to the section below).

**Management API:**

```
POST   /api/accounts/{account_id}/keys
GET    /api/accounts/{account_id}/keys
DELETE /api/accounts/{account_id}/keys/{key_id}
```

The `GET` endpoint returns the key metadata: the ID, the label, the creation time, the expiry time, and the last use time. It never returns the JWT. Only an account owner or an organisation admin can manage a key.

### STS Exchange

A caller exchanges an API key JWT at `/.sts/assume-role-with-web-identity`. This is the same flow as for any other OIDC token (ADR-004):

```
Action=AssumeRoleWithWebIdentity
&WebIdentityToken=<api_key_jwt>
&RoleArn=sc::my-org::role/publisher
&RoleSessionName=ncar-daily-sync
```

The STS exchange flow is the flow of ADR-004 with one more step:

1. Read the `account_id` and the `role_name` from the `RoleArn`.
2. Load the Role definition from the cache.
3. Read the `iss` claim from the JWT. It is `https://data.source.coop`.
4. Check the JWT signature against the JWKS of the proxy.
5. Check `exp` (if it is present), `nbf`, and `iat`.
6. **Check the `jti` in the policy store.** Make sure that no person revoked the key (cached, 30–60 s TTL).
7. Evaluate the claim constraints of the IdP binding that agrees.
8. Make sure that `DurationSeconds` is not more than the `max_session_duration` of the Role.
9. Make the credentials and send the response.

Step 6 is the only addition to the existing STS flow. The proxy omits this step for a token that is not an API key, thus for a token with no `"type": "api_key"` claim.

### Registration as a Platform IdP

The system registers the issuer of the proxy as a platform IdP:

```json
{
  "id": "source-coop-api-key",
  "issuer_url": "https://data.source.coop",
  "display_name": "Source Cooperative API Key",
  "well_known_claims": ["type"],
  "audience_hint": "https://data.source.coop"
}
```

A caller can assume a Role with an API key only if that Role has an identity constraint for this IdP:

```json
{
  "idp": "source-coop-api-key",
  "claim_constraints": [
    {"claim": "type", "operator": "equals", "value": "api_key"}
  ]
}
```

This uses the Role and identity constraint model of ADR-004 with no change. Each account owner permits API key access for each Role separately. A caller cannot assume a Role that has no `source-coop-api-key` binding.

### Role Binding

A user can bind an API key to one Role when the user makes the key. A bound key can assume only that Role. An unbound key can assume each Role of the account that has a `source-coop-api-key` identity constraint.

A bound key does less damage if it leaks, because it can access only what its Role permits. We recommend a bound key for each automated workflow of high value.

### Cache and Revocation Latency

The `jti` check uses the same cache infrastructure as the other policy store lookups (ADR-007):

- An in-process cache with a TTL of 30–60 seconds
- Workers KV as a shared cache tier

Thus a revocation becomes effective in 30 to 60 seconds. This latency is satisfactory for the target use cases, which are long cronjobs and batch pipelines. If a faster revocation becomes necessary, we can rotate the HMAC server secret of ADR-001. This makes all active STS sessions invalid immediately. It causes more disruption, but it is available as an emergency response.

---

## Consequences

**Benefits**

- The design fills the authentication gap for environments with no OIDC and no browser access.
- The proxy gets no new authentication path. An API key JWT uses the existing `/.sts` exchange.
- The design uses the existing OIDC issuer infrastructure of the proxy, thus the signing key and the JWKS of ADR-006.
- The design uses the existing Role and identity constraint model of ADR-004.
- The revocation is explicit and auditable through the `jti` lookup.
- An optional Role binding limits the damage from a leaked key.

**Costs and Risks**

- An API key JWT is a bearer token. Any person with the raw JWT can use it. A user must protect it like a password and keep it in an environment variable or a secret file, and not in the source control.
- The `jti` check adds a policy store dependency to the STS exchange path of an API key token. A cache miss adds latency.
- A key with no expiry stays valid until a person revokes it. If a user loses access to the management UI, for example when the user leaves a university, the key continues to exist. Then an organisation admin must revoke it.
- The signing key of the proxy now has two functions: the outbound federation tokens (ADR-006) and the API key JWTs. A compromise of that key has an effect on both. The key rotation must include both functions.

---

## Alternatives Considered

**Long-lived tokens from Ory** — not possible. `auth.source.coop` is Ory Network, and Ory controls its own signing keys. Source Cooperative cannot make an arbitrary long-lived JWT from the issuer of Ory.

**The OAuth2 client credentials grant** — considered. This grant authenticates an application and not a user. Thus the `sub` claim of the token is the client ID and not a user identity. To map an OAuth2 client back to a Source Cooperative account, we must build a custom service account system on OAuth2.

**Personal access tokens from Ory** — examined. The PAT or API key of Ory Network (`ory_pat_`) gives access to the project admin API, and it does not authenticate an end user. A user-scoped PAT is an [open feature request](https://github.com/ory/kratos/issues/1106) on Ory Kratos and is not available.

**Opaque API keys with a hash check** — considered. The platform makes a random secret, keeps a hash of it, and checks the key when it calculates the hash again. This operates correctly. But it needs a separate validation endpoint or a new authentication path at `/.sts`. The JWT approach prevents this. At the STS layer, an API key is the same as any other OIDC token. Thus there is no new endpoint and no new validation logic, except the `jti` check.

**Long-lived refresh tokens from Ory** — considered as a short-term solution. The user does one `source login` with the device flow and keeps the refresh token. A cronjob then gets a new access token automatically. This needs no new infrastructure. But a refresh token expires, and then the unattended workflow fails silently. This is applicable as a temporary measure, but it is not a durable solution for a workload that continues indefinitely.
