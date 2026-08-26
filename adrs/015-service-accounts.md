# ADR-015: Service Accounts — A Principal for Unattended Software

**Status:** Proposed — not implemented
**Date:** 2026-08-24
**RFC:** RFC-001 §7
**Depends on:** ADR-010, ADR-014
**Blocks:** ADR-013

---

## Context

Source Cooperative can only authenticate people. Getting a credential needs a human at a browser, so anything that runs unattended — a nightly sync, a publishing pipeline, an instrument uploading observations — must either borrow a person's session or store a person's secret somewhere it does not belong.

ADR-010 lets an account create Roles that a CI workflow can assume, which covers part of this. But a Role is a ceiling, not an identity: the permissions it narrows still belong to the account that owns the Role. There is no way to grant access to *the pipeline* as distinct from the person who set it up, to revoke the pipeline without touching that person, or to tell them apart in a log.

ADR-013 assumes an API key's `sub` is a human account id, for the same reason — there is nothing else it could be.

**What is missing is a principal: something that is not a person, can hold grants of its own, and can be revoked on its own.**

---

## Decision

### A Service Account Is an Account

Service accounts reuse the account model rather than adding a parallel one. A service account has three parts:

| Part | Answers | Where it lives |
|---|---|---|
| The account | Who is this? | A new `service` account type |
| Sign-in methods | How does it prove that? | Identity bindings (ADR-014), one or many |
| Grants | What may it reach? | Ordinary memberships, one or many |

Sign-in methods and grants are independent. Adding a second way to sign in does not change what the account may reach, and changing what it may reach does not affect how it signs in. A GitHub workflow and an API key that resolve to the same service account get identical access — if a workflow should have narrower access than a key, make two service accounts.

Ids are minted in a reserved namespace, `svc--<name>`. The existing id pattern forbids a double hyphen anywhere in a human-chosen id, so the prefix cannot be forged.

### Ownership

Each service account names exactly one owner — a person or an organisation. A single owner is required because two rules need one: a service account can never do more than its owner currently can, and disabling the owner disables what it owns.

Where the creator belongs to an organisation, the owner defaults to the organisation. An organisation-owned service account survives any member leaving; a personally-owned one is disabled when that person is. That is the intended safety property, and also an accidental outage if the default is wrong.

The cap is evaluated per request, not frozen at grant time, and it applies to the authorisation **decision** rather than to the membership list — three paths in the API authorise without consulting memberships at all, and a cap applied only to the list would miss them.

> [!NOTE]
> **The owner cap is undefined when the owner is an organisation,** which is also the recommended default. Organisations never authenticate, and the existing membership lookup returns rows where the organisation is the *member*, which is not the same as what it can reach. Either define it as the products the organisation owns plus grants it holds, or state that organisation-owned service accounts are uncapped. This must be settled before the cap is built.

Deleting an account that owns service accounts is blocked. A missing owner denies, rather than skipping the check.

### Scope Is a Restriction on Grant Types

A service account may hold only data grants — read or write on a product, or (per ADR-016) delete. It cannot hold an owner or maintainer role.

This is the whole scope statement, and it needs no separate list of prohibitions. Every other action in `source.coop` — creating products, creating organisations, managing members, owning a record — already requires an owner or maintainer role, so restricting the grant types makes all of them unreachable by construction.

Two guardrails the grant model cannot express stay explicit:

- **The platform admin flag** is read directly from account flags, outside the role system. It needs an invariant at write time, not an omission from the UI.
- **Ids** must be minted in the reserved namespace above.

> [!WARNING]
> **Fix the self-authorisation shortcut first.** `hasRole` returns true before checking any role when the principal's own account id equals the account being acted on, and the read and write data checks do the same against a product's account. A service account has its own account id, so it would authorise itself for anything scoped to itself — including editing its own flags. The owner cap has the same hole. One fix serves both, and it must land before either.

### Roles Are Unchanged

A service account assumes a Role by URN exactly as any other caller does (ADR-010), and the credential is the Role's ceiling intersected with the account's live grants (ADR-011). Nothing here adds a second authorisation model.

Which Roles a service account may assume is recorded on the Role, in its `identity_constraints`, alongside the binding that identifies the account. A management UI may present this as a per-service-account list, but there is one store, not two.

---

## Consequences

**Benefits**

- Unattended software gets an identity that is not a person's, revocable on its own.
- Grants, revocation, and the pages that manage them are the ones that already exist.
- One identity can hold several sign-in methods, so swapping CI for a key is a configuration change rather than a re-grant.
- Logs can finally tell a pipeline apart from the person who configured it.

**Costs / Risks**

- Account type is branched on in roughly 48 places across 20 files in `source.coop`. Most keep compiling and quietly treat a machine as a person. Slightly under half are the binary `isIndividualAccount` / `isOrganizationalAccount` helpers, which would render a machine as an organisation profile rather than failing.
- The membership pages are the deliberate exception and must show service accounts, since that is where an owner revokes a grant. Membership listing currently returns false for any third account type.
- An account id is also the first segment of a public URL, so every service account consumes a name and gets a profile route.
- The owner cap adds an owner lookup to the request path, which is already sensitive to latency.

---

## Alternatives Considered

**A separate table instead of a new account type** — considered, and close. It would make exclusion a compile error rather than 47 judgement calls, and keep machines out of the public account schema. Rejected because grants would then need a parallel membership model, which is a larger and more duplicative change than the branch sites it avoids. If the branch sites prove unmanageable in practice, this is the fallback.

**Permissions attached to each sign-in method** — rejected. It doubles the authorisation surface for a case ("CI may read, the key may write") that two service accounts already express.

**Roles alone, with no principal** — rejected. This is ADR-010 as it stands. A Role narrows an account's permissions but is not an identity, so a pipeline cannot hold a grant of its own, cannot be revoked without touching its owner, and cannot be distinguished in an audit record.

**Per-service-account Role definitions** — rejected. Roles are account-owned and reusable (ADR-010); minting one Role per service account would multiply near-identical definitions and move grant management out of the membership pages that already do it.
