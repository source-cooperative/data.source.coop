# ADR-008: Modular Crate Architecture and Community Reuse Model

**Status:** Proposed
**Date:** 2026-03-14
**RFC:** RFC-001 §11
**Depends on:** ADR-003, ADR-004, ADR-005

---

## Context

The current proxy is tightly coupled to Source Cooperative's specific data model, backend configuration, and operational context. The re-architecture treats Source Cooperative's deployment as *one instance* of a general-purpose S3-compatible data proxy framework. The framework is the primary artefact; Source Cooperative's configuration is a thin layer on top.

This work builds on [`multistore`](https://github.com/developmentseed/multistore), an existing effort to create a composable S3-compatible proxy in Rust.

---

## Decision

### Separation of Concerns via Crates

The proxy is structured as separate Rust crates to promote composability. Concerns like auth, authorization, storage backend resolution, and middleware are separated behind trait boundaries so that they can be developed, tested, and reused independently.

The exact crate boundaries will emerge during implementation. The principle is separation of concerns, not a fixed crate map. Key areas of separation:

- **Request routing and SigV4 verification** — the core proxy mechanics
- **STS exchange and JWT validation** — inbound authentication (ADR-004)
- **Authorization and policy evaluation** — Role ceiling, account permissions (ADR-005)
- **Storage backend resolution** — mapping products to `object_store` configurations
- **Middleware** — request logging and future cross-cutting concerns (ADR-007)
- **Runtime adapters** — Cloudflare Workers (WASM) and traditional server (Hyper/Tokio)

**Nothing Source Cooperative-specific lives in the core crates.** All Source Cooperative-specific behaviour is expressed through the same trait interfaces that any other operator would use.

### Trait-Based Extension Points

Each area of concern defines traits that downstream operators implement. This allows operators to provide their own IdP configurations, policy store backends, storage resolvers, and middleware without forking the core.

### Publication and Licensing

Core crates are intended for publication to `crates.io` under a permissive licence.

> [!NOTE]
> **TODO:** Finalise crate boundaries, naming, and licensing as the implementation progresses.

---

## Consequences

**Benefits**

- Community members can build their own data proxies on the same foundation
- Contributions to the core benefit all deployments
- Clean trait boundaries prevent Source Cooperative-specific concerns from leaking into the framework
- No forking required for custom deployments

**Costs / Risks**

- Maintaining trait stability across crate versions requires discipline and a clear semver policy
- Multiple crates increase build and release coordination overhead
- Trait boundaries must be designed carefully — changing a public trait is a breaking change

---

## Alternatives Considered

**Monolithic crate with feature flags** — considered. Simpler build, but makes it difficult for operators to depend on only the parts they need. Feature flags don't provide the same clean separation as separate crates with trait boundaries.

**Fork-based customisation** — rejected. The current model. Leads to divergent forks that don't benefit from upstream improvements. Trait-based extension is strictly preferable.

**Configuration file instead of trait implementations** — considered. Would allow operators to customise behaviour via YAML/TOML without writing Rust. Rejected: insufficient expressiveness for the range of customisation needed (custom auth flows, custom middleware, custom policy stores). Configuration can complement traits but cannot replace them.
