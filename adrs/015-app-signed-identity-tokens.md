# ADR-015: App-Signed Identity Tokens for Interactive Web Users

**Status:** Proposed
**Date:** 2026-10-04
**Depends on:** ADR-004, ADR-009
**Amends:** ADR-004 ("Identity Sources in Use — Interactive web users")

---

## Context

ADR-004 has the web app obtain its users' proxy credentials by driving an Ory Hydra authorization-code flow server-side: it starts `/oauth2/auth`, accepts the login challenge for the session's identity through the Ory admin API, follows the redirect, accepts consent if Hydra asks, and redeems the code at `/oauth2/token` for an ID token. Only then does it call `/.sts`. That is four to six sequential HTTP calls to Ory before the one call that matters, and none can run in parallel, because each needs the previous redirect or challenge.

This is slow where users notice it. Turning on Edit Mode in the web app waits on this chain, and so does the first view of a restricted product. source.coop#636 trims what can be trimmed on the app side (it reuses credentials the browser already holds, starts the mint when the menu opens, and drops a duplicate session lookup), but the first mint in each hour still pays for the full chain.

The chain is also not a standard grant. The app impersonates the user at the identity provider in order to get a token the proxy will accept. ADR-004 records the cost: "a compromised call site is a 'become any user' primitive", guarded by keeping the function off the Server Action surface. The guard holds, but the capability it guards is the Ory admin API, which can do far more than mint tokens.

The app is already the party that decides who the user is. It verifies the Ory session on every request, and the ID token Hydra returns says nothing the app did not tell Hydra to say: its `sub` is the identity id the app passed to the login accept. Hydra adds a signature, not information.

Two earlier decisions bear on the alternative:

- ADR-005 already has the proxy assert any `sub` to the Source API with its own short-lived signed JWT, verified against the proxy's JWKS. The trust this ADR proposes is the same relationship in the other direction.
- ADR-013 considered "source.coop-signed JWT keys, verified at the proxy via a source.coop JWKS" and called the trust direction right: "the control plane issues, the data plane verifies as it verifies GitHub". It rejected them for *long-lived* API keys, because a key with no expiry obliges the platform to keep every signing key forever, and because a baked-in `exp` conflicts with an editable record. A token that lives five minutes and is never stored has neither problem.

---

## Decision

### The app signs a short-lived identity token

For an interactive user, the web app signs a JWT itself and exchanges it at `/.sts` in place of the Hydra ID token:

| Claim | Value |
|---|---|
| `iss` | The app's issuer for its environment, e.g. `https://source.coop` (production), `https://staging.source.coop` (staging and preview deployments) |
| `aud` | The proxy for that environment, e.g. `https://data.source.coop` |
| `sub` | The Ory identity id from the verified session, the same value the Hydra ID token carries today, so `source_identity` and everything downstream of it are unchanged |
| `iat`, `nbf` | Now |
| `exp` | `iat` + 300 seconds |
| `jti` | Random, for log correlation only; the proxy keeps no replay list |

The header is `alg: RS256` with a `kid`, because ADR-004 pins RS256 on inbound tokens. The token is minted in the same server-only function that drives the Hydra flow today, under the same rule: the `sub` comes from a verified session, never from request input. It is used once, immediately, and never stored or sent to the browser.

### The app publishes its JWKS

The app serves its public keys at `<iss>/.well-known/jwks.json`. The proxy fetches and caches them through the JWKS cache it already uses for Ory and platform issuers (ADR-004). This is a fetch from the Worker to another origin, so the self-fetch restriction that stopped the proxy verifying its own JWKS (ADR-013, error 1042) does not arise.

Preview deployments share the staging environment's issuer and key. The issuer is a configured constant, not the request host, so a preview's per-branch hostname never appears in `iss`.

### The proxy trusts it as a person issuer

The app's issuer joins Ory as an issuer whose tokens **name the person** (the ADR-004 path), not as a platform issuer whose tokens act only as the account `RoleArn` names (the ADR-014 path). This needs the per-issuer audience requirement ADR-009 introduced, applied to person issuers: each person issuer carries its own audience list, and an issuer with no audience is refused, as ADR-004 requires. Ory keeps its client-id audiences for the CLI, and the app's issuer accepts only the proxy's own `aud`.

The proxy logs the token's `iss` with each exchange. This gives the attribution ADR-004 deferred RFC 8693 for: an exchange by the web app on a user's behalf is now distinguishable from the user's own CLI exchange.

### Key management

The private key is a Vercel environment variable for each environment. Vercel bakes variables in at build time, so a rotation is a deploy, in three steps:

1. Publish the next public key alongside the current one, and deploy.
2. Sign with the next key, and deploy.
3. Once the old key's tokens have expired and the proxy's JWKS cache has turned over (5 minutes plus 15 minutes; allow an hour), remove the old public key, and deploy.

Nothing long-lived depends on the key, so a rotation invalidates nothing. Compromise recovery is the same three steps, run without waiting.

### The Hydra flow stays for what needs it

The CLI keeps exchanging its own Ory ID tokens. The app keeps the Hydra flow behind a configuration switch until the new path has run in production, then deletes it.

---

## Consequences

**Benefits**

- The web app's mint drops from five to seven sequential calls to one: the `/.sts` exchange.
- The Ory admin API leaves the credential path. The app still holds the admin key for identity lookups, but no request that mints credentials uses it.
- The signing key can do one thing: assert a person to the proxy, for five minutes. The admin key it replaces on this path can create, edit and delete identities.
- The token says what happened: the app vouched for the user. Today's ID token claims a login that never took place.

**Costs / Risks**

- **The signing key is a "become any user" credential for the data plane.** Anyone who holds it can mint any person's proxy credentials until it is rotated out. That is the same power the Ory admin key confers today, on a narrower surface, but it is a second secret with that power, kept in Vercel.
- The app gains a JWKS route and a key-rotation runbook it does not have today. A rotation takes three deploys.
- The proxy trusts a second person issuer. A misconfigured audience on either issuer fails toward accepting a token meant for another service; ADR-004's per-issuer fail-closed rule is the guard.
- The proxy's availability now depends on reaching the app's JWKS, and on its being correct. A cached JWKS covers short outages; a bad deploy that drops the current key stops all web mints until fixed.

---

## Alternatives Considered

**Cache the Hydra ID token at login, so Edit Mode skips the chain.** Rejected. An ID token can be exchanged at `/.sts` repeatedly until it expires, so a cached one is a credential that mints credentials, and it is worse to leak than the credentials themselves. It would usually have expired by the time the user opened Edit Mode anyway.

**Longer STS sessions, so the chain runs less often.** Rejected. Issued credentials cannot be revoked short of rotating the proxy's signing key (ADR-001), and logging out only clears the app's cookie. Longer sessions trade latency for a longer window on leaked credentials, without making the first mint any faster.

**Hydra's JWT-bearer grant (RFC 7523).** The app would sign an assertion and Hydra would trade it for a token in one call. It still needs the app's own signing key, still makes one Ory call, and Hydra returns an access token rather than an ID token. Once the app has a signing key, letting the proxy trust it directly is strictly simpler.

**OAuth 2.0 Token Exchange (RFC 8693) at Ory.** It is the standard shape for "the app acts for the user", but Ory Network does not offer it. Doing the exchange at the proxy instead would mean a new grant at `/.sts`, which AWS SDKs do not speak, for a result this ADR already gets with a JWT `/.sts` accepts.

**The app's Vercel OIDC token as its identity.** It needs no key management, but it identifies the deployment, not the user, and `AssumeRoleWithWebIdentity` has no parameter to name a user on whose behalf it acts. Inventing one would leave the AWS-compatible contract. It also exists only on Vercel, not in local development.

**Keep the Hydra flow and only make it faster.** Enabling `skip_consent` on the OAuth client saves two calls when consent would otherwise be asked; it should be on regardless. It leaves four Ory calls and the admin API on the credential path.
