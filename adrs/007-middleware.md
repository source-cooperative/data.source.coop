# ADR-007: Middleware Architecture

**Status:** Proposed
**Date:** 2026-03-14
**RFC:** RFC-001 §10
**Depends on:** ADR-005, ADR-008

---

## Context

A general-purpose data proxy needs behaviours beyond authentication and object retrieval — access logging, usage analytics, rate limiting, and cost attribution. These cross-cutting concerns are best implemented as composable middleware wrapping the core request handler.

---

## Decision

### Middleware Stack Pattern

Cross-cutting concerns are implemented as a **composable middleware stack** wrapping the core request handler. Each middleware layer:

- Receives the request context (resolved identity, role, resource, action) and may modify or enrich it
- May short-circuit the request with a denial response
- May record an event (e.g. to a log or metrics store)
- Passes the request to the next layer if permitted

### Middleware as Rust Traits

Middleware components are defined as Rust traits, making them first-class extension points. Source Cooperative ships standard implementations; operators can add their own without forking the core (see ADR-008).

> [!NOTE]
> **Future extension: Access logging and analytics.** The middleware architecture is designed to support structured request logging for usage analytics (which products and files are most popular, which accounts drive the most traffic) and cost attribution (distinguishing open data program buckets, Source Cooperative-owned buckets, and third-party provider-hosted buckets). The log backend, schema, storage, and analytics pipeline are significant decisions that will require a dedicated ADR.
>
> **Future extension: Rate limiting, quotas, and billing.** The following capabilities are deferred until there is concrete demand and a defined operational model:
>
> - **Rate limiting** — per-identity or per-product request rate enforcement
> - **Quota enforcement** — cumulative data transfer tracking with access thresholds
> - **Billing event emission** — publishing usage events to a billing backend
> - **Audit logging** — tamper-evident request logs for compliance
>
> Each of these fits the middleware trait interface and can be added without modifying the core proxy.

### Unresolved

- **Middleware trait interface** — the exact trait signature, including how request context is threaded and how middleware ordering is enforced

---

## Consequences

**Benefits**

- Cross-cutting concerns are composable and configurable, not hardcoded
- New middleware can be contributed by the community without forking the core
- Request logging provides the foundation for usage analysis and debugging from day one
- The trait-based design enforces a consistent interface across all middleware

**Costs / Risks**

- Middleware on the hot path adds per-request overhead (mitigated by keeping middleware lightweight)
- The middleware trait interface is unresolved — implementation cannot begin until it is defined
- Middleware ordering can introduce subtle bugs if order-dependent behaviours are not made explicit

---

## Alternatives Considered

**Hardcoded middleware in the core proxy** — rejected. Does not support the modularity and community-reuse goals. Provider-hosted datasets need different middleware stacks than Source Cooperative's own datasets.

**Sidecar/external middleware (e.g. Envoy filters)** — considered. Offloads middleware to a separate process. Rejected: does not work in the Workers deployment target (no sidecar model), and adds latency from inter-process communication.

**Plugin system (dynamic loading)** — considered. Would allow middleware to be loaded at runtime. Rejected for the Workers target: WASM does not support dynamic library loading. Rust traits with static dispatch are the natural fit for both targets.
