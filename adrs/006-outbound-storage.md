# ADR-006: Outbound Connectivity — OIDC Issuer Model and `object_store` Adoption

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

Upstream cloud providers (AWS, GCP, Azure) register Source Cooperative as a trusted external identity provider via their native workload identity federation mechanisms. The proxy generates short-lived, audience-scoped JWTs and exchanges them for cloud credentials at each provider's STS. The exact `aud` and `sub` values, and the reasoning behind them, are specified in [Outbound Token Contract](#outbound-token-contract--aud-and-sub) below — that contract is the mechanism by which an upstream cloud constrains *which* data connection may use a given role.

This model means:
- No long-lived cloud credentials are stored in the proxy
- Credentials are ephemeral
- The trust relationship is declarative and auditable
- Key rotation at the proxy level propagates automatically without reconfiguring upstream providers

#### Direct Federation vs. Brokered Role Access

There are two ways a third-party data provider can grant the proxy access to their storage:

1. **Direct federation** — The data provider registers Source Cooperative as a trusted OIDC identity provider in their own cloud account and creates a role (or service account, or federated identity) that the proxy can assume directly. This gives the provider full control but requires them to configure IdP trust in their account.

2. **Brokered role access** — Source Cooperative registers itself as an OIDC identity provider in its _own_ cloud account and assumes its own cloud role (e.g. an AWS IAM role, GCP service account, or Azure managed identity). The data provider then grants that Source Cooperative role cross-account access to their storage (e.g. via an S3 bucket policy, GCS IAM binding, or Azure role assignment). The provider never needs to register Source Cooperative as an identity provider — they only need to trust an existing cloud identity.

The brokered model lowers the barrier for data providers: granting a cloud role access to a bucket is a familiar operation, while registering an external OIDC identity provider is not. It also centralises the OIDC configuration to a single place (Source Cooperative's own account) rather than requiring each provider to replicate it. The tradeoff is that the provider must trust Source Cooperative's intermediate role, and Source Cooperative's account becomes a choke point — any misconfiguration or compromise of that role affects all providers who rely on it.

Both models can coexist. Providers with stricter security requirements or existing IdP federation workflows can use direct federation; providers who prefer simplicity can grant access to Source Cooperative's brokered role.

#### Outbound Token Contract — `aud` and `sub`

The outbound token's claims are the only channel through which an upstream cloud can express *which* Source Cooperative data connection may use a given role. This section fixes that contract. It is distinct from the inbound `sub` = `account_id` contract in ADR-005; the two never meet.

**Both claims are derived entirely from server-side state and are never influenced by the requester.**

| Claim | Value | Purpose |
|---|---|---|
| `iss` | `https://data.source.coop` | Fixed. The proxy's OIDC issuer. |
| `sub` | `scv1:conn:{connection_id}` | Stable per-connection identity. Exact-matchable on every cloud. |
| `aud` | **Direct federation:** `scv1:conn:{connection_id}`<br>**Brokered:** a single platform audience | The federation boundary (see below). |

`scv1:` is a versioned prefix; the grammar is a public, stable contract, because customers embed it in their own cloud policies. Changing it is a breaking change for every provider who has configured trust.

**The audience is the primary boundary for direct federation.** Each cloud validates the token's audience against the provider registration *before* any role trust policy or IAM binding is evaluated:

- **AWS** — the `aud` claim must match one of the client IDs registered on the IAM OIDC provider (maximum 100 per provider). A mismatch fails the `AssumeRoleWithWebIdentity` call outright.
- **GCP** — `allowed_audiences` on the workload identity pool provider.
- **Azure** — the federated identity credential's `issuer`, `subject` and `audience` must all match case-sensitively.

So a provider registers exactly one audience — their own connection's:

```
aws iam create-open-id-connect-provider \
  --url https://data.source.coop \
  --client-id-list scv1:conn:acme--acme-bucket
```

A token minted for any *other* connection carries a different audience and is rejected by the cloud before policy evaluation. **The provider is therefore protected even if their role trust policy carries no conditions at all.** This is deliberate: it makes the safe configuration the default rather than something the provider must remember to add, and a provider debugging a failed exchange cannot accidentally disable the boundary by removing a condition. Providers may additionally pin `sub`, and are encouraged to, but correctness does not depend on it.

Do **not** use `sts.amazonaws.com` as the audience. It is only the convention adopted by GitHub Actions and EKS IRSA; a shared audience makes every connection's token interchangeable at the provider-registration layer and pushes the entire boundary onto trust-policy conditions.

**Scope belongs in `sub`, not in custom claims.** For a generic OIDC provider, AWS exposes only a fixed set of condition keys — `amr`, `aud`, `email`, `oaud`, `sub` — and ignores all other claims. A trust policy therefore cannot condition on a custom `account` or `product` claim, however convenient that would be. Any scope an upstream policy must see has to be encoded in `sub`.

**Keep `sub` a stable per-connection identity; do not append variable scope to it.** Azure matches `subject` exactly, with no wildcard support, so a subject that varies per product or per account would require the provider to pre-register one federated credential per value. AWS and GCP tolerate pattern matching, but a design that only works on two of three clouds is not portable. Where a finer boundary is genuinely required, express it as a *separate scoped subject on a Source-owned role* (see below), not by decorating the customer-facing one.

**The brokered model needs a different mechanism.** A cloud account may register only one OIDC provider per issuer URL, so Source Cooperative's own provider must accept the audiences of *all* platform connections — audience validation cannot discriminate among them. The brokered role's trust policy must therefore constrain `sub` directly, as an **allowlist** of permitted platform subjects:

```jsonc
"StringLike": { "data.source.coop:sub": "scv1:conn:aws-opendata-*" }
```

Use an allowlist, never a negated condition. In AWS IAM, a negated string condition whose context key is *absent* evaluates to **true**; a `StringNotLike` on `sub` is safe only when paired with a positive condition on the same key that fails closed, which is a fragile property for a security boundary to rely on. Audience validation and the `sub` allowlist are complementary: the former secures direct federation, the latter secures brokered access.

#### Credential Cache Keying

Exchanged credentials are cached, and the cache key must be exactly as coarse as the isolation boundary and no coarser. Two invariants:

1. **Key on `(role_identifier, sub)`, never on the role identifier alone.** Keying on the role alone lets a connection whose subject the trust policy would reject be served a credential minted for one it accepts, short-circuiting the exchange so no upstream policy is ever consulted.
2. **`aud` must remain a pure function of data already in the key.** It is, because `aud` is derived from the connection ID and `sub` contains it. If the audience ever varies independently of the subject, it must be added to the key.

The same rule governs any future per-request scoping. **If a per-request STS session policy is introduced, a fingerprint of that policy must be added to the cache key in the same change** — otherwise two requests sharing `(role, sub)` but differing in resolved prefix collide, and one is served a credential scoped to the other's prefix. That failure is a silent authorization bypass that defeats the mechanism being added.

Prefer expressing prefix confinement declaratively where the cloud allows it. On AWS the `sub` condition key is available in session, so a **bucket policy** can require that the session's subject match the prefix being accessed — achieving the same enforcement as a session policy with no size limit, no per-request computation, and no cache-key interaction. A session policy remains available as defence in depth.

### Outbound Authentication — Stored Credentials (Fallback)

The current proxy fetches static cloud credentials (access key ID and secret access key) from the Source Cooperative API for each data connection. The API stores these credentials and serves them to the proxy on demand, cached with a short TTL.

For upstream providers or storage systems that do not support OIDC workload identity federation, this model continues: the proxy fetches stored credentials from the API and uses them to authenticate to the upstream backend. This is not a preferred path — stored credentials must be rotated manually, create a larger blast radius if compromised, and require the platform to hold long-lived secrets on behalf of providers. Data providers should be encouraged to configure OIDC trust relationships where their cloud supports it.

Notable backends that **do not** support external OIDC identity federation for storage access (and therefore require stored credentials):

- **Cloudflare R2** — API tokens or access key pairs only; no mechanism to trust an external OIDC issuer for storage operations
- **Backblaze B2** — Application keys only; no STS or federation mechanism
- **Wasabi** — Supports STS `AssumeRole` for its own IAM users, but OIDC integration is limited to console SSO, not storage API federation from an external identity provider
- **DigitalOcean Spaces** — No support for trusting an external OIDC issuer; workload identity is limited to DigitalOcean's own internal Droplet-issued tokens

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
- Provider credential isolation for **direct federation** is resolved by the per-connection audience: a connection's token is only accepted by the provider that registered that audience. Isolation under the **brokered** model still rests on the `sub` allowlist in Source Cooperative's own trust policy, and on request-time prefix resolution, so the brokered role remains the choke point noted above
- The `scv1:` subject and audience grammar is a public contract embedded in provider-side cloud policies. Changing it breaks every configured provider, so it must be versioned and treated as an API
- Rotation of the proxy's signing keys is handled by JWKS publication, but rotation of a *connection's* identity (its audience) requires the provider to update their IdP registration — this is a coordinated change, not a unilateral one

---

## Alternatives Considered

**Manual per-backend adapters (current model)** — rejected. Maintenance-intensive, creates ongoing integration gaps, and does not scale with new backends.

**Provider-managed proxy instances** — considered. Each data provider runs their own proxy instance with their own credentials. Rejected: fragments the platform, complicates access control, and defeats the purpose of a unified distribution layer.

**Proxy stores all upstream credentials in a secrets manager (e.g. AWS Secrets Manager)** — considered as the primary model rather than fallback. Rejected in favour of OIDC: secrets managers still store long-lived credentials that must be rotated. OIDC federation eliminates stored secrets entirely for providers that support it.

**A single shared outbound audience (e.g. `sts.amazonaws.com`), with isolation enforced only by `sub` conditions in each provider's trust policy** — rejected. It is the conventional choice and the simplest to implement, but it makes every connection's token interchangeable at the provider-registration layer, so a provider is protected only if they author a correct condition. That fails open: a provider who omits the condition, or removes it while debugging a failed exchange, silently grants access to every connection on the platform. A per-connection audience moves the boundary to a step the provider cannot skip.

**Encoding account or product scope in custom JWT claims** — rejected as unimplementable on AWS, which exposes only `amr`, `aud`, `email`, `oaud` and `sub` as condition keys for a generic OIDC provider and ignores all other claims.

**Encoding account or product scope as suffixes on the customer-facing `sub`** — rejected. Azure matches `subject` exactly with no wildcards, so a varying subject would require one pre-registered federated credential per value. It also multiplies credential-cache cardinality, and any scope suffix risks truncation in `RoleSessionName` (64 characters), which degrades CloudTrail attribution precisely where it is most needed.
