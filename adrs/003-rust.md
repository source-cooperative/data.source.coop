# ADR-003: Rust as Implementation Language

**Date:** 2026-03-14
**RFC:** RFC-001 §6
**Language:** ASD-STE100 Simplified Technical English

---

## Context

The new proxy must compile to WebAssembly for Cloudflare Workers (ADR-002). The language must also compile the same codebase to a native target, for future deployment targets. The proxy does operations that are sensitive to security: it checks cryptographic signatures, issues credentials, and evaluates access policies.

The current proxy is in Rust. The contributors to Source Cooperative know Rust better than Go, and Go better than C++. More people know Python, but Python is not applicable to the WASM target.

---

## Decision

We continue to use **Rust** as the implementation language.

### Rationale

**WASM maturity.** Rust has the most mature toolchain for compilation to WebAssembly. The `worker-rs` crate gives idiomatic bindings to the Cloudflare Workers runtime. This is a known path, and not a bet on a new capability.

**Performance.** Rust has zero-cost abstractions and no garbage collection pauses. Thus Rust is applicable to a proxy that transfers large objects and has tight latency limits. The current proxy already showed this.

**Type system and correctness.** The proxy operates on authentication tokens, credential issue, cryptographic signatures, and access policies. The type system of Rust, and specially its trait system, holds invariants that other languages can only check at runtime. This is more and more important in a codebase where AI helps to write the code. A strong type system finds generated code that compiles but that breaks the domain constraints.

**Extensibility through traits.** The trait system of Rust is the basis of the modular design of multistore. Traits let the core framework specify interfaces for authentication, authorization, storage backends, middleware, and configuration. Downstream users then write their own implementations and do not fork the core.

**Community knowledge.** Rust is the best fit for the actual group of contributors.

---

## Consequences

**Benefits**

- One codebase compiles to WASM and to native targets.
- Zero-cost abstractions and no GC pauses give a high throughput for streams.
- The trait system makes the modular architecture possible, and the community can extend it.
- The strong type system finds errors in code that is sensitive to security.
- The work continues on the existing proxy. The current contributors do not learn a new language.

**Costs and Risks**

- New contributors learn Rust more slowly than Go or Python.
- Compilation takes more time than in Go.
- The WASM target limits which crates and which `std` features the shared core can use.
- The async runtime is different for Workers (`worker-rs` primitives) and for native targets (`tokio`). If we add more deployment targets, we must abstract this difference carefully.

---

## Alternatives Considered

**Go** — considered. Its WASM support increases but is less mature than the Rust support. It has no trait system for the modular design. Its GC pauses are a risk for streams with a high throughput. Also, fewer Rust contributors must learn a new language than Go contributors.

**TypeScript (the native language of Workers)** — considered. Workers support it fully, but its performance for streams is limited. Its types cannot hold the security invariants that the ownership and trait system of Rust can hold.

**Python** — rejected. It does not compile to WASM. Its runtime overhead is too large for a streaming proxy.

**C++** — rejected. Fewer contributors know it than Rust. Memory safety is a risk in code that is sensitive to security. It has no equivalent trait system for extensibility.
