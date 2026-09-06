# ADR-014: Federated Subject Resolution — `(issuer, subject)` as the Account Key

**Status:** Proposed — not implemented
**Date:** 2026-08-24
**RFC:** RFC-001 §7
**Depends on:** ADR-004, ADR-005
**Blocks:** ADR-009, ADR-010, ADR-015

---

## Context

ADR-005 records that the proxy signs its API calls with the caller's Ory identity id, and that `source.coop` looks that id up to find the account. The lookup runs against a single index and then filters the result to individual accounts, so it can only ever return a person.

That is fine while there is one issuer and every caller is a human. ADR-009 adds more issuers, ADR-010 needs organisations to be the subject, and ADR-015 adds machines. All three break on the same lookup.

There is a second problem, and it is a security one. The credential the proxy mints records **who** the caller is but not **who vouched for them**. Every issuer's subjects therefore share one flat namespace. If a second issuer can be told what subject to put in a token — which is the whole point of letting users register their own — it can name a subject belonging to someone else, and the API cannot tell the difference. The subject is also part of the proxy's cache key, so a collision poisons cached authorisation results too.

ADR-009 states that its migration step 1 alone "makes CI/CD workflows functional". That is true and not safe: GitHub is a second issuer, and it is subject to exactly this collision.

**The blocker is specific: an account can only be found by a bare subject string, and that string is not qualified by who issued it.**

---

## Decision

### Accounts Are Found by a Pair

An account is resolved by `(issuer, subject)`, never by `subject` alone. The existing single-argument lookup becomes a thin wrapper that supplies the Source issuer.

### Identity Bindings

A binding records that one subject, from one issuer, is one account:

| Field | Notes |
|---|---|
| `issuer` | The issuer URL exactly as it appears in the token's `iss` claim |
| `subject` | The `sub` claim, matched exactly — never a prefix or a pattern |
| `account_id` | The account this identity is |

The pair `(issuer, subject)` is unique. Two bindings claiming the same pair is a hard error, not a last-writer-wins race: today's lookup takes the last row the index returns, which is not deterministic.

An account may have many bindings. A person has one; a service account (ADR-015) may have several.

> [!IMPORTANT]
> **Attaching a binding requires proof of control of that subject.** Otherwise anyone can claim another organisation's CI subject and receive their access. For GitHub the proof is a token minted by the workflow itself, carrying a one-time audience. For an issuer that cannot mint on demand, registration is reviewed rather than self-serve.

### Issuer Provenance Reaches the Decision

The minted credential already carries `assumed_role_id`. The proxy rewrites the principal it sends to the API into a namespaced form derived from the **verified** issuer — not from the Role, because one Role may trust several issuers and would collapse them back into one namespace.

The API resolves that namespaced principal through the binding table. Cache keys use the namespaced form, so two issuers can no longer share a cache entry.

### Migration

Every existing account must keep working, so this ships in four steps:

1. Add the new index. Write the namespaced attribute on account creation **and update**, or new rows silently miss it.
2. Backfill every existing account to `(Source issuer, Ory identity id)`.
3. Read from the new index, falling back to the old one on a miss.
4. Once the backfill is verified complete in production, remove the fallback.

> [!WARNING]
> **A missed row is a person who cannot log in.** The failure is silent — the resolver returns nothing and the request 401s. Do not remove the fallback in step 4 until a count confirms every account carries the new attribute. Local fixtures and the local-development table definition need the same change, or local development breaks.

---

## Consequences

**Benefits**

- Organisations and service accounts can be the subject of a token, which ADR-010 and ADR-015 both require.
- Two issuers can no longer be confused for one another, in authorisation or in the cache.
- ADR-009 becomes safe to enable, rather than merely functional.
- Every credential can be attributed to the issuer that vouched for it, which is the missing half of ADR-011's audit record.

**Costs / Risks**

- A full-table backfill on live account data, with no migration framework to lean on and a silent failure mode.
- A dual-read window in which two indexes must agree.
- One more index to define in both the deployed table and the local one.
- Proof of control is easy for GitHub and awkward for issuers that cannot mint a token on demand.

---

## Alternatives Considered

**Keep resolving by bare subject** — rejected. It works only while there is one issuer, and ADR-009 removes that condition. The collision is not theoretical: a user-registered issuer chooses its own subject strings.

**Make `sub` an account id instead of an identity id** — considered, and this is the open question ADR-010 leaves. Rejected because it answers a narrower question: it lets an organisation be the subject but still cannot tell two issuers apart, so ADR-009 would remain unsafe. A binding keyed on the pair does both.

**Namespace the subject using the Role's configured issuer list** — rejected. `trusted_oidc_issuers` is a list, so a Role trusting two issuers would map both into one namespace and reintroduce the collision. Derive the namespace from the verified `iss` claim.
