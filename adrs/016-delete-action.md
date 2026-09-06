# ADR-016: Deletion Is Part of Write

**Status:** Proposed — not implemented
**Date:** 2026-08-24
**RFC:** RFC-001 §8
**Depends on:** ADR-005, ADR-010, ADR-011

---

## Context

The platform has no way to say "may upload, may not delete".

The proxy classifies actions with a denylist: anything that is not `GetObject`, `GetObjectVersion`, `HeadObject` or `ListBucket` is a write. Every write is then gated on a single string, `write`, returned by the Source Cooperative API, whose permission vocabulary is two values. So `DeleteObject` and `PutObject` are indistinguishable at every layer, for people as well as for machines.

"Append-only publisher" is a reasonable thing to want, particularly for unattended software (ADR-015). The question is whether it needs a third permission value.

---

## Decision

### `write` includes deletion

A grant of `write` on a product permits deletion. There is no third permission value in the API, no new grant type in the UI, and no migration of existing grants.

Two reasons.

**A writer can already destroy data without `DeleteObject`.** `PutObject` on an existing key overwrites it. Withholding deletion does not protect the data from a hostile or badly broken writer; it makes them take one extra step. Durability against that comes from versioning or object lock, not from splitting an action.

**The vocabulary is public.** `RepositoryPermissions` is part of the Source Cooperative API. Adding a third value obliges every consumer to understand it, and obliges us to decide what existing `write` grants map to — where the only safe answer, `write` implies `delete`, grants nothing anyone asked for.

### Roles may subset it

Where "may upload, never delete" is genuinely wanted, it is expressed as a Role rather than as a grant.

A Role carries a per-action list and can only subtract (ADR-011), so a Role whose actions omit `DeleteObject` yields credentials that cannot delete, no matter what the account's memberships allow. That gets the capability with no change to the permission vocabulary, no migration, and nothing new for a product owner to understand.

This is deferred work, not work this ADR schedules: the Roles that ship are the two built-ins (ADR-010, scope note), and neither omits deletion. It becomes available when account-owned Roles do.

> [!IMPORTANT]
> **`AbortMultipartUpload` must stay with `write`.** It removes an incomplete upload, which is cleanup any writer has to be able to perform. A workload that cannot abort its own failed multipart uploads leaves orphaned parts accruing storage cost. If a Role ever subtracts destructive actions, this must not be in that set.

### Fix the classifier

`GetObjectVersion` was added in multistore 0.7.2 and is not listed as a read, so version reads are currently gated as writes. The denylist is failing safe, but it is still wrong. Correct it.

The denylist stays a denylist: an action that is not explicitly a read is treated as a write, so a new action added upstream is over-restricted until classified, never under-restricted.

---

## Consequences

**Benefits**

- No change to a public API vocabulary, and no migration of existing grants.
- Product owners keep a two-value model — read, or read and write — which is the thing they actually reason about.
- Least privilege for deletion stays available, through the mechanism built for narrowing.

**Costs / Risks**

- `aws s3 sync --delete` remains the realistic accident: a pipeline meant only to add files removes half a product. Nothing in this decision prevents it until account-owned Roles ship.
- "May write but not delete" is unavailable in the meantime, so anyone asking for it today gets no answer.
- The distinction lives in two vocabularies that must agree — the API's `read`/`write`, and a Role's per-action list. ADR-011 already names divergence between them as a silent correctness risk.

---

## Alternatives Considered

**A third permission value, `delete`** — rejected. It reads as stronger protection than it is, since `PutObject` can already destroy an object by overwriting it. It also spends a public API change and a migration on a guarantee that only holds against accidents, and Roles cover the accident case without either.

**Express deletion only in Role permission statements, and say nothing in the grant model** — accepted, and this is what the decision above amounts to. Recorded separately because the choice is easy to misread as an oversight.

**Versioning or object lock on the backing store** — deferred, and the right answer for durability. It protects against overwrite as well as deletion, which no permission split does. Out of scope here because it is a storage-configuration decision per data connection, not an authorization one.
