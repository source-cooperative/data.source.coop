# ADR-013: API Keys for Environments Without OIDC

**Status:** Proposed — not implemented (revised 2026-09-25)
**Date:** 2026-04-01 · revised 2026-09-25
**RFC:** RFC-001 §12
**Depends on:** ADR-001, ADR-004, ADR-007, ADR-014
**Amends:** ADR-005 (proxy-to-API authentication), ADR-014 (the amendment of this ADR)

> [!NOTE]
> The `api-keys` endpoints that exist in `source.coop` today are the **legacy** admin-managed keys used by the pre-Workers proxy. They are unrelated to this design, and the current proxy has no code path that accepts them (ADR-001). This ADR proposes a replacement, not a formalisation of what is there.

> [!NOTE]
> **Revised 2026-09-25.** The original Decision — a long-lived JWT signed by the data proxy — was implemented (source-cooperative/data.source.coop#233, source-cooperative/source.coop#570) and withdrawn on review before release. The Decision below replaces it; the original design and why it was withdrawn are recorded under Context and Alternatives. ADR-014's amendment of this ADR (the key belongs to one service account; no per-key Role binding; editable expiry, several active keys) carries over.

---

## Context

ADR-004 defines inbound authentication via OIDC federation: callers present a JWT from a trusted identity provider and exchange it at `/.sts` for short-lived STS credentials. This works well for CI/CD platforms with ambient OIDC tokens (GitHub Actions, GitLab CI, etc.) and for interactive users who can complete a browser-based login via `auth.source.coop`.

However, a significant class of users has neither:

- Researchers running recurring batch jobs or cronjobs on university HPC clusters (SLURM, PBS, traditional login nodes)
- On-premises instruments or data loggers that push observations on a schedule
- Legacy ETL systems in environments without a supported OIDC issuer

These users have Source Cooperative accounts but operate in compute environments that do not issue OIDC tokens and cannot perform interactive browser authentication at runtime. ADR-001 and ADR-004 both identify this gap as future work.

### Why the first Decision was withdrawn

The first version of this ADR chose to issue API keys as long-lived JWTs signed by the data proxy, on the grounds that such a key could be exchanged at `/.sts` "with no new endpoint and no new validation logic beyond the `jti` check". ADR-014 kept that shape and made the key's subject a service account.

Implementing it (source-cooperative/data.source.coop#233, developmentseed/multistore#147, source-cooperative/source.coop#570) showed the premise does not hold:

- **The lookup was never optional.** Revocation needs a per-key check at the Source API on every exchange, cached for 60 seconds. A lookup keyed by the secret itself returns the account, so the signature's only remaining job, naming the account before the lookup, is one the lookup can do.
- **The existing path could not be used.** A Cloudflare Worker cannot fetch its own JWKS (error 1042), so the proxy verified its own keys in process, ahead of the STS route, with a dedicated role and error mapping, plus a new `POST /.keys` for minting. That is the new endpoint and the new validation logic the JWT was meant to avoid.
- **Minting became a cycle.** source.coop wrote the record, obtained an Ory ID token, called the proxy, and the proxy called source.coop back to confirm the caller manages the account, then signed. `/.keys` would sign any `jti` for a manager, so a manager could re-derive a live key for an existing record with no audit; "shown once" was not a property.
- **Two sources of truth for expiry.** `exp` was baked into the token; the record's `expires_at` was editable. Extending or removing an expiry did not change what the proxy enforced.
- **A rotation cliff.** The proxy's signing key also signs outbound federation assertions (ADR-006) and its own calls to the API (ADR-005). The proxy verified keys against the current and one previous signer. A key with no expiry, which ADR-014 promises, stopped verifying on the second rotation, and any rotation forced by the other two uses invalidated every key at once.

A design comment on the epic (source-cooperative/source.coop#491, 2026-08-22) had already stated the principle: a long-lived credential "must not depend on a signature staying verifiable for years". That comment drew the two-hop conclusion revisited under Alternatives.

---

## Decision

### The key is an opaque secret

An API key is `sck_` followed by 32 random bytes in base64url, a fixed 47 characters matching `^sck_[A-Za-z0-9_-]{43}$`, with no checksum. source.coop generates it, stores `sha256(key)` on the key record, shows it once, and never stores or logs the key. Nothing signs it. There is no key material for API keys anywhere on the platform. The hash needs no salt or key-derivation function: its input is 256 random bits, and the lookup is a key get, so there is no comparison to time.

The record holds `key_hash` (partition key), `key_id` (random, public, for the UI and management actions), `account_id` (a service account, per ADR-014), `label`, `created_at`, `created_by`, `expires_at` (nullable, editable after issuance), `revoked_at` and `last_used_at`. A service account may hold several active keys; rotation is issue-new, deploy, revoke-old. A disabled service account is refused a key.

### The proxy resolves it by asking source.coop

`/.sts` accepts a key as `WebIdentityToken` in an `AssumeRoleWithWebIdentity` request, exactly as it accepts a JWT. The proxy:

1. Accepts a key **only from the form body of a POST**. A key anywhere in the query string is refused before any lookup, with a message that says so, because the platform logs request URLs.
2. Trims surrounding whitespace, then checks the fixed format; a malformed value is refused locally.
3. Hashes the key and looks up its standing at `POST /api/v1/service-account-keys/exchanges` with `{"key_hash"}`, authenticated as the proxy itself (see Amendments). The answer, `{account_id, key_id, active}` with `active: false` for an unknown hash, is cached for 60 seconds. An API failure fails closed and caches nothing.
4. Refuses an inactive or unknown key with one client-visible outcome, `InvalidIdentityToken` "API key was not accepted (request id …)", the id in the message because SDKs surface only the message; the reason is in the log.
5. Otherwise mints session credentials for `account_id` through the same minting, sealing and response code as every other exchange, with the same duration floor and cap. The account segment of `RoleArn` is ignored, as it is for an Ory ID token; the key names the account. The role must be one the proxy serves.

source.coop answers `active` only when the key is not revoked, not expired, and its service account is not disabled, and records last use best-effort.

Exchange attempts are rate-limited by client IP with the Workers rate-limiting binding, generously: a legitimate client exchanges about once a session, so a cluster behind one NAT stays far under the limit, while a flood of distinct junk keys, each of which costs the API one lookup, is what it bounds.

### Clients

A stock AWS SDK or CLI needs `AWS_ROLE_ARN`, `AWS_WEB_IDENTITY_TOKEN_FILE` pointing at a file containing the key, `AWS_ENDPOINT_URL_STS`, `AWS_ENDPOINT_URL_S3` and `AWS_REGION`. The SDK re-reads the file, exchanges, and refreshes on its own; nothing else runs on the machine. This is the same setup GitHub Actions uses with a real GitHub token. The Source CLI offers the same exchange with the key in the request body and can serve as an AWS `credential_process`, which is how tools that would otherwise send the STS request as a GET, such as GDAL, obtain credentials: the proxy refuses a key in a URL, and GDAL honours `credential_process` in the AWS config.

### Revocation

Revoking a key, or its expiry passing, denies new exchanges within the 60-second standing cache. Session credentials already issued cannot be recalled; they live to their cap. **Disabling the service account** is the emergency stop for a leaked key: new exchanges are refused, and existing sessions degrade to anonymous as the proxy's per-request caches expire, writes within 60 seconds and reads of restricted products within five minutes; public reads are unaffected. Re-enabling makes any leaked keys live again. Rotating `SESSION_TOKEN_KEY` (ADR-001) invalidates every session on the platform and is not part of the key runbook.

A public self-revoke route lets anyone holding a key revoke it (source-cooperative/source.coop#561): the same hash lookup, body-only, a uniform response, the same rate limit.

---

## Amendments

### ADR-005 — Authorization delegated to the Source API

ADR-005 rejected "a service-account identity for proxy-to-API calls" and authenticates every lookup as the caller. One route is the exception: `POST /api/v1/service-account-keys/exchanges` is called before the proxy knows which account is calling, so the proxy authenticates it **as itself**, with a proxy-signed assertion whose subject is the sentinel `urn:source:data-proxy`. That subject fails both account-id grammars, is accepted only on that route, and resolves to no account anywhere else. Every other lookup stays on behalf of the caller.

### ADR-014 — Service accounts

The first two bullets of ADR-014's amendment of this ADR are replaced: a key is an opaque secret whose record names the service account; revocation is per key, by record, not by `jti`. The remaining bullets, no per-key Role binding and editable expiry with several active keys, stand.

---

## Consequences

**Benefits**

- One source of truth. Existence, account, expiry, revocation and disablement are all the record's, read by one lookup the proxy already had to make.
- No signing key on the key path. The proxy's key stays reserved for outbound federation and its API calls; rotating it cannot affect an API key. Non-expiring keys need no retained key ring.
- Issuance is one DynamoDB write, with no cross-service call and no compensating delete.
- The proxy never holds a key at rest and forwards only its hash. Enumeration is infeasible at 256 bits.
- The stock-SDK experience the epic promises for GitHub Actions holds for every environment.
- The secret-scanning marker is a fixed-length, all-entropy pattern.

**Costs / Risks**

- `/.sts` gains a second verification branch ahead of the STS route. It reuses the STS crate's minting, sealing and response builders, but restates the duration floor and default in a few lines because the crate has no pre-resolved-subject entry point.
- A long-lived secret rides in `WebIdentityToken`, a parameter defined for signed assertions. It is accepted only in a POST body over TLS and hashed on arrival. Clients that send the STS request as a GET are refused and must go through the CLI.
- The exchanges route is the first Source API route that authenticates the proxy as itself. It is confined to that route and its sentinel subject.
- This is the first `/.sts` path where an unverified caller triggers a Source API call, so the rate limit ships with the branch, not after it.
- Revocation is bounded by the 60-second cache, and outstanding session credentials by their cap and by the per-request caches. Both are true of every credential the proxy issues.
- The public self-revoke route is a second surface that takes a raw key, under the same rules.

---

## Alternatives Considered

**Proxy-signed JWT keys (this ADR's first Decision)** — withdrawn for the reasons in Context: the lookup makes the signature redundant, the platform prevented reuse of the JWT path, and the shared signing key created an issuance cycle, a re-mint hole, an expiry conflict and a rotation cliff.

**source.coop-signed JWT keys, verified at the proxy via a source.coop JWKS** — the trust direction is right (the control plane issues, the data plane verifies as it verifies GitHub), and it avoids the self-JWKS problem. It keeps the lookup, the `exp`-versus-record conflict and the obligation to retain every signing key forever, now on Vercel where secrets are baked at build. It buys nothing over an opaque key that the lookup does not already provide.

**Two-hop: opaque key exchanged at source.coop for a short-lived token, then `/.sts`** — the most conventional shape (OAuth 2.0 client credentials; AWS IAM Roles Anywhere), and the conclusion of the 2026-08-22 design comment. A spike against the staging Ory project on 2026-09-25 (run output in source-cooperative/data.source.coop#234) showed Ory Network mints an ID token for a service-account subject through the headless flow source.coop already uses, so the proxy would need no change. Rejected for the first release because it charges its cost to the users this ADR exists for: every HPC node, instrument and VM would need the Source CLI as a credential helper or a timer rewriting a token file, where this design needs environment variables and a stock SDK. It also widens the revocation window from 60 seconds to the token lifetime and puts Ory's admin API on the machine path. It remains available as an addition, without changing the key format, should a use case need a JWT derived from a key.

**Long-lived credentials via the proxy's `get_credential` slot** — rejected in the same design comment: SigV4 is symmetric, so the proxy would need the secret in the clear.

**Ory personal access tokens; OAuth 2.0 client credentials at Ory directly** — not available for end users, and a client is not an account (retained from the first version).

**Long-lived Ory refresh tokens** — retained from the first version: a one-time `source login` whose refresh token cronjobs use silently. Refresh tokens expire eventually, causing silent failures in unattended workflows; an interim measure, not a durable one.
