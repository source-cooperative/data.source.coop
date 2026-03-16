# ADR-007: Middleware Architecture — Rate Limiting, Metering, and Billing Hooks

**Status:** Pending
**Date:** 2026-03-14
**RFC:** RFC-001 §10
**Depends on:** ADR-005, ADR-008

---

## Context

A general-purpose data proxy needs behaviours beyond authentication and object retrieval. Source Cooperative specifically requires:

- **Rate limiting** — fine-grained and dynamic: different limits per bucket, per user, per organisation, or per role
- **Data metering** — tracking cumulative data transfer per identity or dataset, and enforcing access thresholds (e.g. denying access once a monthly quota is reached)
- **Usage tracking and billing hooks** — recording access events with enough fidelity to support downstream billing
- **Audit logging** — a complete, tamper-resistant record of who accessed what and when

These concerns are cross-cutting: they apply to every request regardless of the specific storage backend or dataset, but their configuration and behaviour differ across deployments and use cases. Provider-hosted datasets may carry additional metering and quota requirements beyond what Source Cooperative's own datasets need.

---

## Decision

### Middleware Stack Pattern

Cross-cutting concerns are implemented as a **composable middleware stack** wrapping the core request handler. Each middleware layer:

- Receives the request context (resolved identity, role, resource, action) and may modify or enrich it
- May short-circuit the request with a denial response (e.g. quota exceeded, rate limit hit)
- May record an event (e.g. to a metering store or audit log)
- Passes the request to the next layer if permitted

### Middleware as Rust Traits

Middleware components are defined as Rust traits, making them first-class extension points. Source Cooperative ships standard implementations; operators can add their own without forking the core (see ADR-008).

### Configuration Scope

The middleware stack is configured per-deployment and potentially per-dataset. A dataset with no billing requirements carries a lightweight stack; a provider-hosted dataset with metered access carries additional quota and event-recording middleware.

### Standard Middleware (Planned)

| Middleware | Behaviour |
|---|---|
| Rate limiter | Per-identity or per-bucket request rate enforcement, configurable limits |
| Quota enforcer | Cumulative data transfer tracking; deny on threshold exceeded |
| Usage recorder | Structured event emission per request (bytes transferred, identity, resource, latency) |
| Audit logger | Tamper-evident request log for compliance and forensics |
| Billing emitter | Usage event publication to a configurable billing backend |

### Unresolved

The following details require further design:

- **Middleware trait interface** — the exact trait signature, including how request context is threaded and how middleware ordering is enforced
- **Per-dataset configuration** — how middleware stacks are expressed per-deployment and per-bucket
- **Event schema** — the structured format for usage recording and billing events
- **Event backend** — the initial target for event emission (Kinesis stream, S3/R2 log, webhook, or other). See RFC-001 Open Question 6
- **Middleware ordering** — whether order-dependent behaviours are made explicit or left to the operator

---

## Consequences

**Benefits**

- Cross-cutting concerns are composable and configurable, not hardcoded
- New middleware can be contributed by the community without forking the core
- Per-dataset middleware stacks support the data provider hosting model
- The trait-based design enforces a consistent interface across all middleware

**Costs / Risks**

- Middleware on the hot path adds per-request overhead (mitigated by keeping middleware lightweight)
- Per-dataset middleware configuration adds operational complexity
- The middleware trait interface, event schema, and event backend are all unresolved — implementation cannot begin until these are defined
- Middleware ordering can introduce subtle bugs if order-dependent behaviours are not made explicit

---

## Alternatives Considered

**Hardcoded middleware in the core proxy** — rejected. Does not support the modularity and community-reuse goals. Provider-hosted datasets need different middleware stacks than Source Cooperative's own datasets.

**Sidecar/external middleware (e.g. Envoy filters)** — considered. Offloads middleware to a separate process. Rejected: does not work in the Workers deployment target (no sidecar model), and adds latency from inter-process communication.

**Plugin system (dynamic loading)** — considered. Would allow middleware to be loaded at runtime. Rejected for the Workers target: WASM does not support dynamic library loading. Rust traits with static dispatch are the natural fit for both targets.
