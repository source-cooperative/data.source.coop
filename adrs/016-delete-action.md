# ADR-016: `delete` as an Action Distinct from `write`

**Status:** Proposed — not implemented
**Date:** 2026-08-24
**RFC:** RFC-001 §8
**Depends on:** ADR-005, ADR-010, ADR-011

---

## Context

The platform has no way to say "may upload, may not delete".

The proxy classifies actions with a denylist: anything that is not `GetObject`, `HeadObject` or `ListBucket` is a write. Every write is then gated on a single string, `write`, returned by the Source Cooperative API. The API's permission vocabulary is two values, read and write. So `DeleteObject` and `PutObject` are indistinguishable at every layer, for people as well as for machines.

That is a reasonable simplification for human collaborators and a poor one for unattended software, where "append-only publisher" is the common and sensible shape. ADR-010 anticipates this — its permission statements carry `actions`, with a note that finer actions can be added later — and ADR-011 lists agreeing on what read and write mean as a cost.

**The extension point exists. The decision does not.**

---

## Decision

### A Third Permission Value

`delete` joins `read` and `write` in the API's permission vocabulary, is derived alongside them by the product permissions endpoint, and is checked by the proxy's write gate for destructive actions only.

`DeleteObject` and `AbortMultipartUpload` require `delete`. Every other non-read action continues to require `write`.

### Both Layers, Deliberately

The action set also appears in a Role's permission statements (ADR-010), which are sealed into the credential at mint time (ADR-011). Sealing makes the ceiling checkable with no network call, but it also means a sealed ceiling goes stale for up to a session.

So `delete` lives in both places, and they do different jobs:

| Layer | Job | Revocation lag |
|---|---|---|
| Role permission statements | The ceiling. Survives a bug in the live path. | Up to one session |
| API permission lookup | The live grant. | Roughly 60 seconds |

Putting `delete` only in the sealed ceiling would mean "we revoked delete" takes an hour. Putting it only in the live lookup would leave the ceiling unable to express least privilege, which is the point of ADR-010.

### The Classifier Keeps Failing Safe

The denylist stays a denylist: an action that is not explicitly a read is still treated as a write, and an action that is not explicitly destructive is not treated as a delete. A new action added upstream is gated as a write until it is classified, never the reverse.

> [!NOTE]
> **`GetObjectVersion` is currently misclassified.** multistore 0.7.2 added it, and the proxy's classifier does not list it as a read, so version reads are gated as writes today. Correct this with the same change — it is the denylist failing safe, but it is still wrong.

---

## Consequences

**Benefits**

- "Upload but never delete" becomes expressible, for service accounts and people alike.
- Least privilege for publishing pipelines, which is the shape most of them want.
- Revoking delete takes effect as fast as revoking write.

**Costs / Risks**

- A third value in a vocabulary two codebases and the public API already agree on. Existing grants must map to something, and the safe mapping — existing `write` implies `delete` — preserves behaviour but grants nothing new that anyone asked for.
- One more thing for a Role author to get wrong.
- The UI question is unsettled: is `delete` a grant a product owner ticks, or only something a Role can subtract? A grant type is more expressive and more work.

---

## Alternatives Considered

**Leave delete inside write** — rejected. It is the status quo, and it makes the most common unattended shape inexpressible.

**Express delete only in Role permission statements** — rejected. Sealed ceilings mean revocation waits out the session. Delete is the action where a slow revocation matters most.

**A full S3 action vocabulary** — deferred. Ten actions exist upstream, but only the destructive split has a demand behind it. Adding more later is backwards-compatible; carrying nine unused permission values is not free.
