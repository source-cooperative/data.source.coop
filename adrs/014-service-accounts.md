# ADR-014: Service Accounts

**Status:** Proposed — implemented in part (source-cooperative/source.coop#563–#567)
**Date:** 2026-09-21
**RFC:** RFC-001 §7
**Depends on:** ADR-004, ADR-005, ADR-009
**Amends:** ADR-010 (scope), ADR-013 (subject and Role binding)

---

## Context

Source Cooperative cannot authenticate software acting on a user's behalf. Getting credentials requires a person at a browser, so anything unattended — a nightly sync, a publishing pipeline, an instrument uploading observations — either babysits a login or embeds a person's session where it does not belong.

ADR-010 answers part of this. An account-owned Role carries identity constraints ("who may assume it") and a permission ceiling, so a CI workflow can obtain a credential *narrower* than the account that authored the Role. But a Role only ever subsets the owning account's permissions. Two things follow:

- **There is no principal whose access is separate from a person's or an organisation's.** A workflow assuming an organisation's Role acts with a subset of the organisation's access; revoking it means editing the Role, and widening the organisation's access silently widens what the workflow can reach.
- **An organisation cannot be a subject.** ADR-010's "Organisation Subject Problem" records that `source.coop` resolves a token's subject through an individual's identity index, that organisations never authenticate, and that every invitation path rejects a non-individual account. Making organisations authenticate would touch the most sensitive code in the API without giving automation a permission set of its own.

What an unattended workload needs is a principal with **its own grant**: revocable without touching a person, never inheriting a person's broader access, and able to be a *member* of products the way a person is.

---

## Decision

### A third account type

`service` joins `individual` and `organization`. A service account is owned by exactly one account, individual or organisation (`owner_account_id`), and is managed by whoever manages the owner — the owner's `owners` and `maintainers`, or an individual owner themselves. It has no Ory identity and no public profile. It never acts as admin whatever its flags say, and it creates neither products nor accounts. It has no rights over itself: the self-authorization shortcut that lets a person edit their own account is a person's alone.

No reserved id namespace. `type` is the discriminator and `owner_account_id` records ownership; the id is an ordinary account id.

### How it authenticates: identity bindings

An account is found by *how it signed in*. `source.coop` holds an `identity-bindings` table keyed `(issuer, subject) → account_id` — the pair is the key, so a subject binds to one account per issuer and nothing more. An individual's Ory identity is not a binding: it stays on the account row as `identity_id`, which the session, the email lookup and the proxy credentials already read, and resolves through that index. The table holds subjects the platform cannot derive from an account: a service account is bound under whichever platform IdPs (ADR-009) it integrates with, one binding per exact subject. An API key's subject is the service account's own id (ADR-013), which the API resolves directly, so a key writes no binding either.

**Attaching a binding requires proof of control of the subject.** For GitHub Actions: whoever manages the service account names the exact subject — one repository and one ref or one environment, never organisation-wide — and receives a short-lived signed challenge. The workflow proves it controls that subject by minting its ambient OIDC token with the challenge as the audience and posting it back; the token is verified against GitHub's keys *for that audience*, its subject must equal the challenge's, and only then is the binding written. Without proof, anyone could claim another organisation's CI subject and receive its access.

The token path is then: the proxy verifies a token from a trusted platform IdP, forwards the **issuer-qualified** subject to the API (source-cooperative/data.source.coop#222), and the API resolves `(issuer, subject)` through the bindings table to an account of any type — the Ory issuer excepted, which resolves through `identity_id`. This is how the Organisation Subject Problem is resolved: the subject of a workload's credential is the *service account*, not the organisation that owns it.

### What it may reach: memberships

A service account holds permissions as ordinary memberships — the same rows, and the same revocation, as a person's. Two rules narrow them:

- Only `read_data` or `write_data`, never `owners` or `maintainers`. A machine does not manage people or products.
- Only on products its owner owns, and only per product. Organisation-wide grants, and grants on another account's products, are deferred.

Its owner grants access directly, as a member: there is nobody at the keyboard to accept an invitation.

### Roles still apply, as ceilings

ADR-010 and ADR-011 are unchanged in kind. A service account names a Role when it asks for credentials, and the Role only ever subtracts from its memberships. Today two Roles are hardcoded, `FullAccess` and `ReadOnly`, with `_default` kept as an alias (source-cooperative/data.source.coop#221); when account-owned Roles arrive, a service account assumes a Role its owner authored, exactly as ADR-010 describes.

The division of labour: **a Role answers "how narrow is this credential"; a service account answers "whose grant is this".**

---

## Amendments

### ADR-013 — API keys

- The `sub` of an API-key JWT is a **service account** id, not an arbitrary account id. A key belongs to one service account; an individual or organisation does not hold keys directly.
- Enabling keys on a service account writes no binding: the key's subject is the account id itself, and the API resolves it as a service account by id after trying Ory. Whether an account has keys is the keys table's to say; revocation stays per key, by `jti`.
- **No per-key Role binding in the first release.** The service account's memberships are the grant, and the hardcoded Roles only subtract, so any key may name either. The dependency on ADR-010 is dropped; ADR-013 depends on this ADR instead.
- Expiry is optional and may be changed after issuance; a service account may hold several active keys, so rotation is overlap by construction.

### ADR-010 — account-owned Roles

- **Scope:** account-owned Roles — CRUD, per-Role trust policies, user-authored permission statements, the API lookup on the credential path — are deferred. Two hardcoded Roles ship in their place (source-cooperative/data.source.coop#221).
- The Organisation Subject Problem is resolved by this ADR rather than by making organisations authenticate: the subject is the service account.
- When account-owned Roles do land, their identity constraints and a service account's bindings are not redundant. A binding says which subjects *are* this account; a Role's constraints say which of an account's subjects may assume *this* ceiling.

---

## Consequences

**Benefits**

- Automation gets a grant of its own — revocable in one place, never inheriting a person's broader access, and visible on the same membership pages as a person's.
- Organisations can own automation without becoming subjects themselves.
- Multi-issuer trust (ADR-009) becomes safe to enable per subject: a GitHub token maps to a service account with exactly the memberships it was given, not to a person's whole account.
- The bindings table is the store every later issuer resolves against; GitLab, Azure DevOps and the rest are one proof-of-control flow each.

**Costs / Risks**

- A new account type touches every place that branches on the existing two — around fifty sites — and the default at each is *exclude*.
- Two lookups by design, not one: an Ory identity resolves through `identity_id`, everything else through a binding. Until source-cooperative/data.source.coop#222 qualifies the subject with its issuer, the proxy forwards a bare `sub` and the API tries Ory first, so a service account whose id equals a person's Ory identity id would resolve to the person; such an account is refused a key.
- Proof of control is a new subsystem per issuer, and its weakest point is the challenge's key handling.
- Each service account consumes a public name; a per-owner cap is an open question.
- Deleting an owner that owns service accounts must be blocked (account deletion is itself unimplemented, source-cooperative/source.coop#355).

---

## Alternatives Considered

**ADR-010 Roles alone** — rejected. A Role subsets its owner's permissions; it cannot give automation a grant that is separate from, and revocable independently of, a person's or an organisation's. And an organisation cannot be a subject.

**Let organisations authenticate and hold memberships** — rejected. Every authentication and invitation path filters to individuals, so this is the same amount of work as a new type, and it still does not give the organisation's automation a permission set *separate* from the organisation's.

**OAuth2 client credentials** — ADR-013 already rejected it as requiring "a bespoke service account system". This ADR is that system, built on the account model rather than beside it.

**A reserved `svc--` id namespace** — rejected. `ID_REGEX` forbids consecutive hyphens in account ids, and `--` is already the data-connection composite-id delimiter; relaxing the rule would let user-chosen ids collide with connection ids. The account type is the discriminator.

**Roles selectable per service account ("tick which Roles it may use")** — rejected for the first release. A Role can only subtract, so any caller may safely name either hardcoded one; a tick-box would be a no-op that reads as a restriction.
