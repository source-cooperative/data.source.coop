# ADR-006: Outbound Connectivity — OIDC Issuer Model and `object_store` Adoption

**Status:** Proposed
**Date:** 2026-03-14
**RFC:** RFC-001 §9
**Depends on:** ADR-002

---

## Context

When the proxy receives an authenticated, authorised request, it must retrieve or write the underlying object from an upstream storage backend (S3, GCS, Azure Blob, R2, etc.). This outbound connection must itself be authenticated, without embedding long-lived cloud credentials in the proxy service.

The current proxy implements per-backend adapters manually — a separate integration for each cloud storage provider, with bespoke error mapping from each provider's client library. This is maintenance-intensive and creates an ongoing gap as new backends are added or existing client APIs change.

Additionally, Source Cooperative intends to support **data providers** who register their own upstream storage with the platform. The proxy fronts their buckets with auth, authz, rate limiting, and metering.

---

## Decision

### `object_store` as Unified Storage Abstraction

The [`object_store`](https://crates.io/crates/object_store) crate replaces all manual per-backend adapters. `object_store` provides a single async trait (`ObjectStore`) with implementations for S3, GCS, Azure Blob, R2, HTTP, and local filesystem.

This eliminates backend-specific client code and error mapping from the proxy codebase. New storage backends supported by `object_store` become available without proxy changes.

### Outbound Authentication — OIDC Token Issuance (Preferred)

Source Cooperative operates as an OIDC identity provider, publishing:
- `/.well-known/openid-configuration` — OIDC discovery document
- A JWKS endpoint — public keys for verifying tokens issued by the proxy

Upstream cloud providers (AWS, GCP, Azure) register Source Cooperative as a trusted external identity provider via their native workload identity federation mechanisms. The proxy generates short-lived, audience-scoped JWTs and exchanges them for cloud credentials at each provider's STS.

This model means:
- No long-lived cloud credentials are stored in the proxy
- Credentials are ephemeral
- The trust relationship is declarative and auditable
- Key rotation at the proxy level propagates automatically without reconfiguring upstream providers

### Outbound Authentication — Stored Credentials (Fallback)

The current proxy fetches static cloud credentials (access key ID and secret access key) from the Source Cooperative API for each data connection. The API stores these credentials and serves them to the proxy on demand, cached with a short TTL.

For upstream providers or storage systems that do not support OIDC workload identity federation, this model continues: the proxy fetches stored credentials from the API and uses them to authenticate to the upstream backend. This is not a preferred path — stored credentials must be rotated manually, create a larger blast radius if compromised, and require the platform to hold long-lived secrets on behalf of providers. Data providers should be encouraged to configure OIDC trust relationships where their cloud supports it.

### Data Provider Hosting

Data providers register their upstream storage (their own S3 bucket, GCS bucket, etc.) with Source Cooperative. The proxy serves as an access control, metering, and distribution layer in front of their data.

Data providers get:
- **Cost control** — rate limiting, metering, and access thresholds prevent runaway egress costs
- **Access control** — fine-grained role and policy configuration
- **Exposure** — data is discoverable via the Source Cooperative platform and UI
- **Outbound auth flexibility** — the provider's own cloud credentials (or OIDC trust relationship) are used for the proxy's outbound connection

---

## Consequences

**Benefits**

- Backend-specific client code and error mapping eliminated from the proxy codebase
- New `object_store` backends available to the proxy without changes
- Preferred outbound auth model uses no long-lived credentials
- Data providers can register their own storage and benefit from Source Cooperative's access control and distribution layer

**Costs / Risks**

- `object_store` must compile to `wasm32-unknown-unknown` for the Workers target — any features that don't work in WASM must be avoided or patched
- The OIDC issuer model requires upstream cloud providers to register Source Cooperative as a trusted IdP — this is a per-provider setup step
- Fallback stored secrets reintroduce long-lived credentials for providers that lack OIDC federation support
- Provider credential isolation and rotation model is unresolved

---

## Alternatives Considered

**Manual per-backend adapters (current model)** — rejected. Maintenance-intensive, creates ongoing integration gaps, and does not scale with new backends.

**Provider-managed proxy instances** — considered. Each data provider runs their own proxy instance with their own credentials. Rejected: fragments the platform, complicates access control, and defeats the purpose of a unified distribution layer.

**Proxy stores all upstream credentials in a secrets manager (e.g. AWS Secrets Manager)** — considered as the primary model rather than fallback. Rejected in favour of OIDC: secrets managers still store long-lived credentials that must be rotated. OIDC federation eliminates stored secrets entirely for providers that support it.
