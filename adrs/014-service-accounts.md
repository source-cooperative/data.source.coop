# ADR-014: Service Accounts

**Status:** Proposed — implemented in part (source-cooperative/source.coop#563–#567)
**Date:** 2026-09-21
**RFC:** RFC-001 §7
**Depends on:** ADR-004, ADR-005, ADR-009
**Amends:** ADR-004 (the `RoleArn` account segment), ADR-005 (who a subject may be), ADR-010 (scope), ADR-013 (subject and Role binding)

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

A service account's id is namespaced under its owner: `{owner_account_id}--{id}`, such as `acme--nightly-sync`. The `--` is one no person's or organisation's id may contain, and the one account-owned data connections already use between owner and name. So the id is unique per owner, and every owner can have its own `nightly-sync`. A service account never takes a handle a person or organisation might want, and its id can never equal an Ory identity id, which is a UUID. `type` stays the discriminator and `owner_account_id` records ownership; the prefix only repeats it.

### How it authenticates: account trusts

An account says which subjects may act as it, the way an AWS role's trust policy does. `source.coop` holds an `account-trusts` table keyed by the account, with one row per issuer and exact subject the account trusts. A subject may be trusted by any number of accounts; nothing about a subject alone chooses an account. An individual's Ory identity is not a trust: it stays on the account row as `identity_id`, which the session, the email lookup and the proxy credentials already read. An API key's subject is the service account's own id (ADR-013), which the API resolves directly, so a key writes no trust either. The table holds what the platform cannot derive from an account: a service account's trust in whichever platform-IdP subjects (ADR-009) it integrates with — for GitHub Actions, one repository pinned to one ref or one environment, never organisation-wide.

**A trust is written when a manager adds it; nothing has to prove control of the subject first.** The workload names the account it wants when it exchanges its token — the account segment of `RoleArn`, `arn:aws:iam::<service-account-id>:role/FullAccess` (or `ReadOnly`, or the `_default` alias). The partition stays `aws`: `aws-actions/configure-aws-credentials`, which is how a workflow is meant to obtain credentials, treats any other partition as a bare role name, and SDKs check only the value's length. The action validates the credentials it exports with `GetCallerIdentity`, which the proxy answers once developmentseed/multistore#126 lands — and the exchange succeeds only if that account trusts the token's issuer and subject. Trusting a subject one does not control gains nothing: its workflows never ask for the account. This is AWS's model, and the flow people already know from integrating GitHub Actions with AWS.

The token path is then: the proxy verifies a token from a trusted platform IdP, reads the account named in `RoleArn`, and asks the API — `POST /api/v1/accounts/{id}/trusts/exchanges`, authenticated as that account — whether it trusts the token's issuer and subject (source-cooperative/data.source.coop#222, #223). Yes means credentials carrying the account's memberships; no means denied. For an Ory ID token the account segment is ignored, because the token itself says who the person is. This is how the Organisation Subject Problem is resolved: the subject of a workload's credential is the *service account*, not the organisation that owns it.

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
- When account-owned Roles do land, their identity constraints and a service account's trusts are not redundant. A trust says which subjects may *be* this account; a Role's constraints say which of an account's subjects may assume *this* ceiling.

---

## Consequences

**Benefits**

- Automation gets a grant of its own — revocable in one place, never inheriting a person's broader access, and visible on the same membership pages as a person's.
- Organisations can own automation without becoming subjects themselves.
- Multi-issuer trust (ADR-009) becomes safe to enable per subject: a GitHub token maps to a service account with exactly the memberships it was given, not to a person's whole account.
- The trusts table is the store every later issuer checks against; GitLab, Azure DevOps and the rest are one subject grammar each, with nothing to prove.

**Costs / Risks**

- A new account type touches every place that branches on the existing two — around fifty sites — and the default at each is *exclude*.
- Two paths by design, not one: an Ory identity resolves through `identity_id`; a service account is named by the caller and checked against its trusts. The proxy forwards a bare `sub` and the API tries Ory first; a service account's id always contains `--` and an Ory identity id never does, so the two cannot be confused.
- A trust is only as narrow as its subject: the platform pins GitHub subjects to one repository and one ref or environment, and every later issuer needs the same care.
- A service account's id takes no public name, since it lives under its owner's, and the owner is fixed for good: it is part of the id. A per-owner cap is an open question.
- Deleting an owner that owns service accounts must be blocked (account deletion is itself unimplemented, source-cooperative/source.coop#355).

---

## Alternatives Considered

**ADR-010 Roles alone** — rejected. A Role subsets its owner's permissions; it cannot give automation a grant that is separate from, and revocable independently of, a person's or an organisation's. And an organisation cannot be a subject.

**Let organisations authenticate and hold memberships** — rejected. Every authentication and invitation path filters to individuals, so this is the same amount of work as a new type, and it still does not give the organisation's automation a permission set *separate* from the organisation's.

**OAuth2 client credentials** — ADR-013 already rejected it as requiring "a bespoke service account system". This ADR is that system, built on the account model rather than beside it.

**A reserved `svc--` id prefix** — rejected. It marks the type, which `type` already does, and it keeps every id platform-wide: one owner's `svc--nightly-sync` is every owner's. Namespacing by owner uses the same `--` to scope the id instead. Loosening the id rule for everyone was the concern; it loosens for service accounts only, whose ids the platform composes from two ids that each pass the strict rule, and connection ids live in their own table.

**Globally unique, un-namespaced ids** — rejected. The create form derives the id from the name, so the second owner to name a service account "Nightly Sync" is told `nightly-sync` is taken, and each one spends a handle a person or organisation might later want.

**Roles selectable per service account ("tick which Roles it may use")** — rejected for the first release. A Role can only subtract, so any caller may safely name either hardcoded one; a tick-box would be a no-op that reads as a restriction.
