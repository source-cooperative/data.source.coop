# ADR-008: Modular Crate Architecture and Community Reuse Model

**Status:** Pending
**Date:** 2026-03-14
**RFC:** RFC-001 §11

---

## Context

The current proxy is tightly coupled to Source Cooperative's specific data model, backend configuration, and operational context. It is difficult for external operators to deploy a version of the proxy for their own datasets, and equally difficult for contributors to improve the proxy in ways that are reusable outside Source Cooperative's deployment.

The re-architecture treats Source Cooperative's deployment as *one instance* of a general-purpose S3-compatible data proxy framework. The framework is the primary artefact; Source Cooperative's configuration is a thin layer on top.

This requires a clean separation between the general-purpose proxy framework and Source Cooperative-specific concerns. All Source Cooperative-specific behaviour must be expressed through the same trait interfaces that any other operator would use.

---

## Decision

### Crate Structure

The proxy is structured as a set of Rust crates with well-defined trait boundaries between layers:

| Crate | Responsibility | SC-specific? |
|---|---|---|
| `proxy-core` | Request routing, SigV4 verification, session credential management, middleware stack execution | No |
| `proxy-auth` | STS exchange logic, OIDC issuer registry, JWT validation, SC Credential Token minting | No — issuer list is configuration |
| `proxy-authz` | Role resolution, per-request policy evaluation, policy store interface trait | No — store backend is pluggable |
| `proxy-storage` | `object_store`-based backend abstraction | No |
| `proxy-middleware` | Middleware trait definition and standard implementations (rate limiter, quota enforcer, usage recorder, etc.) | No |
| `proxy-workers` | Cloudflare Workers runtime adapter, WASM build target | No |
| `proxy-ecs` | Traditional server runtime adapter, Hyper/Tokio based | No |

**Nothing Source Cooperative-specific lives in the core crates.** An operator building their own proxy instantiates the core with their own implementations of the configuration traits — providing their own backend resolver, role mapping, middleware stack — without forking any crate.

Source Cooperative's own deployment is the reference implementation of this pattern.

### Trait-Based Extension Points

Each layer defines traits that downstream operators implement:

- **Auth:** issuer registry, claim condition evaluator, role mapper
- **Authz:** policy store, grant resolver
- **Storage:** backend resolver (maps bucket ID to `object_store` configuration)
- **Middleware:** middleware trait for custom cross-cutting concerns
- **Configuration:** configuration source trait for deployment-specific settings

### Publication and Licensing

Core crates are intended for publication to `crates.io` under a permissive licence.

### Unresolved: Governance

The following governance questions are unresolved:

- Crate naming conventions
- Licence choice (MIT, Apache-2.0, or dual)
- API stability guarantees (semver policy, MSRV policy)
- Whether community-contributed crates live in the same repository, a separate organisation, or are fully external
- What "supported" means for community-contributed middleware or backends
- Contribution model and review process

These are tracked in RFC-001 Open Question 8.

---

## Consequences

**Benefits**

- Community members can build their own data proxies on the same foundation
- Contributions to the core (new middleware, new storage backends, auth improvements) benefit all deployments
- Source Cooperative's infrastructure demonstrates the framework's capabilities, aiding adoption
- Clean trait boundaries prevent Source Cooperative-specific concerns from leaking into the framework
- No forking required for custom deployments

**Costs / Risks**

- Maintaining trait stability across crate versions requires discipline and a clear semver policy
- Multiple crates increase the build and release coordination overhead
- Trait boundaries must be designed carefully upfront — changing a public trait is a breaking change
- Community governance and contribution model are unresolved

---

## Alternatives Considered

**Monolithic crate with feature flags** — considered. Simpler build, but makes it difficult for operators to depend on only the parts they need. Feature flags don't provide the same clean separation as separate crates with trait boundaries.

**Fork-based customisation** — rejected. The current model. Leads to divergent forks that don't benefit from upstream improvements. Trait-based extension is strictly preferable.

**Configuration file instead of trait implementations** — considered. Would allow operators to customise behaviour via YAML/TOML without writing Rust. Rejected: insufficient expressiveness for the range of customisation needed (custom auth flows, custom middleware, custom policy stores). Configuration can complement traits but cannot replace them.
