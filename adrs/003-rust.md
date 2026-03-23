# ADR-003: Rust as Implementation Language

**Date:** 2026-03-14
**RFC:** RFC-001 §6

---

## Context

The re-architected proxy must compile to WebAssembly for Cloudflare Workers (ADR-002). The language must also support native compilation from the same codebase to enable future deployment targets. The proxy handles security-sensitive operations: cryptographic signature verification, credential issuance, and access policy evaluation.

The current proxy is written in Rust. The Source Cooperative contributor community has more Rust experience than Go, and more Go experience than C++. Python is more widely known but is unsuitable for the WASM target.

---

## Decision

We continue with **Rust** as the implementation language.

### Rationale

**WASM maturity.** Rust has the most mature and production-ready toolchain for compiling to WebAssembly. The `worker-rs` crate provides idiomatic bindings to the Cloudflare Workers runtime. This is a well-trodden path, not a bet on emerging capability.

**Performance.** Rust's zero-cost abstractions and lack of garbage collection pauses make it well-suited to a proxy that streams large objects with tight latency requirements. This was already proven by the current proxy.

**Type system and correctness.** The proxy handles authentication tokens, credential issuance, cryptographic signature verification, and access policy evaluation. Rust's type system — and in particular its trait system — encodes invariants that would be runtime errors in other languages. This is increasingly valuable in a codebase where AI-assisted development is part of the workflow: a strong type system provides a correctness harness that catches generated code that compiles but violates domain constraints.

**Trait-based extensibility.** The Rust trait system is central to the modularity goals described in ADR-008. Traits allow the core proxy framework to define interfaces — for auth, authz, storage backend, middleware, configuration — that downstream users implement without forking the core.

**Community familiarity.** Rust is the best fit given the actual pool of contributors.

---

## Consequences

**Benefits**

- Single codebase supports WASM and native compilation targets
- Zero-cost abstractions and no GC pauses for high-throughput streaming
- Trait system enables the modular, community-extensible architecture
- Strong type system as a correctness harness for security-sensitive code
- Continuity with the existing proxy — no rewrite learning curve for current contributors

**Costs / Risks**

- Steeper learning curve for new contributors compared to Go or Python
- Longer compilation times than Go
- WASM target constrains which crates and `std` features can be used in the shared core
- Async runtime differs between Workers (`worker-rs` primitives) and native targets (`tokio`), requiring careful abstraction if additional deployment targets are added

---

## Alternatives Considered

**Go** — considered. Strong WASM support is emerging but less mature than Rust's. Lacks the trait system needed for the modularity goals. GC pauses are a concern for high-throughput streaming. Fewer Rust contributors would need to learn a new language than Go contributors.

**TypeScript (native Workers language)** — considered. First-class Workers support, but limited performance for streaming workloads. No type-level enforcement of security invariants comparable to Rust's ownership and trait system.

**Python** — rejected. Does not compile to WASM. Runtime overhead unsuitable for a streaming proxy.

**C++** — rejected. Less community familiarity than Rust. Memory safety concerns for security-sensitive code. No comparable trait system for extensibility.
