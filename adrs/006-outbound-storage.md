# ADR-006: Outbound Connectivity — `object_store` Adoption and AWS Web-Identity Federation

**Status:** Accepted — implemented
**Date:** 2026-03-14
**RFC:** RFC-001 §9
**Depends on:** ADR-002
**Implementation:** `src/backend_auth.rs`, `src/authz.rs`, `src/source_api/types.rs`, `src/source_api/registry.rs`, `src/lib.rs`
**Implemented by:** #132 (proxy as OIDC provider), #147 (per-connection backend authentication via OIDC federation), #191 / #193 (GCS backend), #172 (bound the outbound STS call), #197 (fix federated reads hanging on a shared credential cache) · source.coop#332 (federated backend authentication), source.coop#368 (provider↔auth pairing, ARN validation), source.coop#376 (secret-less federated config), source.coop#394 (unsigned connections must be read-only), source.coop#398 (bulk-apply web identity to open-data connections), #212 (honour an explicit S3 endpoint)

---

## Context

When the proxy receives an authorised request, it must retrieve or write the underlying object from an upstream storage backend. This outbound connection must itself be authenticated, without embedding long-lived cloud credentials in the proxy service.

The previous proxy implemented per-backend adapters manually — a separate integration per cloud provider, with bespoke error mapping from each client library. This is maintenance-intensive and creates an ongoing gap as backends are added or client APIs change.

Source Cooperative also intends to support **data providers** who register their own upstream storage. The proxy fronts their buckets with auth, authz, and metering.

---

## Decision

### `object_store` as Unified Storage Abstraction

The [`object_store`](https://crates.io/crates/object_store) crate, via multistore, replaces all manual per-backend adapters. It provides a single async trait with implementations for S3, GCS, Azure Blob, R2, HTTP, and local filesystem.

`DataConnectionDetails::backend_options` maps a connection's `provider` to a backend type and its options in one place:

| Provider | Backend | Options derived |
|---|---|---|
| `s3` | `s3` | `bucket_name`, `region`, and an `endpoint` — the connection's explicit `endpoint` when set, otherwise derived from the region |
| `az`, `azure` | `az` | `account_name`, `container_name` |
| `gcs`, `gs` | `gcs` | `bucket_name` |

Any other provider value is rejected rather than guessed at — though as an `Internal` error, so the caller sees a generic 500 rather than the clean client-safe code the auth path returns.

An explicit `endpoint` must win over the region-derived form: S3-compatible backends such as R2 carry `region: "auto"`, which would otherwise derive an unresolvable `s3.auto.amazonaws.com` and fail at DNS. That was #212.

### The Proxy as an OIDC Provider

Source Cooperative publishes an OIDC discovery document and a JWKS endpoint. Upstream clouds register it as a trusted external identity provider, and the proxy mints short-lived assertions to exchange for cloud credentials — so no long-lived cloud credentials are stored in the proxy.

Signing keys are RSA (RS256), configured as `OIDC_PROVIDER_KEY` with `OIDC_PROVIDER_KID`. An optional `OIDC_PROVIDER_KEY_PREVIOUS` / `OIDC_PROVIDER_KID_PREVIOUS` pair is published in JWKS but not used for signing, giving zero-downtime key rotation: consumers pick up the new key before the old one is withdrawn. A previous key set without its `kid` is omitted from JWKS with a warning rather than served ambiguously.

The same signing identity authenticates the proxy to the Source Cooperative API (ADR-005).

### Per-Connection Backend Authentication

Each data connection declares how the proxy should authenticate to it. The model is internally tagged and **fails closed on anything it does not recognise**:

| `authentication.type` | Behaviour |
|---|---|
| absent / `null` / `unsigned` | Public bucket — issue an unsigned request (`skip_signature`) |
| `s3_web_identity_role` | Mint an assertion, exchange it at AWS STS, sign with the result |
| anything else | `UnsupportedAuthType` — deny |

A malformed or unknown `authentication` deserialises leniently to `Unsupported` rather than failing the whole response — one bad connection must not break parsing of a list — and is then **denied at use**. Falling back to unsigned is specifically what must not happen: it could expose a backend that is only anonymously readable by accident.

`s3_web_identity_role` on a non-S3 backend is rejected as `ProviderMismatch`, rather than injecting AWS options into an Azure or GCS request that would fail opaquely downstream.

### Outbound Token Contract — `sub`

The outbound token's claims are the only channel through which an upstream cloud can express *which* data connection may use a given role. This contract is distinct from the inbound subject in ADR-005; the two never meet.

**Both claims are derived entirely from server-side state and are never influenced by the requester.**

| Claim | Value | Purpose |
|---|---|---|
| `iss` | `OIDC_PROVIDER_ISSUER` (`https://data.source.coop` in production) | The proxy's OIDC issuer. Per-environment, not a constant — staging issues under its own host, which is why non-production federation needs the canonical identity. |
| `sub` | `scv1:conn:{connection_id}` | Stable per-connection identity. Exact-matchable on every cloud. |
| `aud` | `sts.amazonaws.com` | AWS's fixed web-identity convention; constant across connections. |

`scv1:` is a versioned prefix, and the grammar is a public, stable contract: customers embed it in their own cloud policies. Changing it is a breaking change for every provider who has configured trust.

**Scope belongs in `sub`, not in custom claims.** For a generic OIDC provider, AWS exposes only a fixed set of condition keys — `amr`, `aud`, `email`, `oaud`, `sub` — and ignores all other claims. A trust policy cannot condition on a custom `account` or `product` claim, however convenient that would be.

**Keep `sub` a stable per-connection identity; do not append variable scope to it.** Azure matches `subject` exactly with no wildcards, so a subject varying per product or account would require one pre-registered federated credential per value. AWS and GCP tolerate patterns, but a design that works on two clouds of three is not portable.

**Trust policies must constrain `sub` as an allowlist:**

```jsonc
"StringLike": { "data.source.coop:sub": "scv1:conn:aws-opendata-*" }
```

Never a negated condition. In AWS IAM, a negated string condition whose context key is *absent* evaluates to **true**; a `StringNotLike` on `sub` is safe only when paired with a positive condition on the same key that fails closed — a fragile property for a security boundary to rest on.

> [!IMPORTANT]
> With a shared `sts.amazonaws.com` audience, **the `sub` allowlist is the only boundary between connections.** A provider whose trust policy omits it accepts tokens minted for any connection on the platform. ADR-012 proposes a per-connection audience to make the safe configuration the default instead of something the provider must remember to add.

### Credential Cache Keying

Exchanged credentials are cached, and the cache key must be exactly as coarse as the isolation boundary and no coarser:

1. **Key on `(role_identifier, sub)`, never the role alone.** Keying on the role alone lets a connection whose subject the trust policy would reject be served a credential minted for one it accepts, short-circuiting the exchange so no upstream policy is ever consulted.
2. **`aud` must remain a pure function of data already in the key.** Today it is constant, so this holds trivially. ADR-012 makes `aud` connection-derived; `sub` already contains the connection ID, so the invariant survives — but it must be re-checked in that change.

**If a per-request STS session policy is ever introduced, a fingerprint of that policy must be added to the cache key in the same change** — otherwise two requests sharing `(role, sub)` but differing in resolved prefix collide, and one is served a credential scoped to the other's prefix. That is a silent authorization bypass that defeats the mechanism being added.

Prefer expressing prefix confinement declaratively where the cloud allows it: on AWS the `sub` condition key is available in session, so a **bucket policy** can require the session subject to match the prefix being accessed — the same enforcement with no size limit, no per-request computation, and no cache-key interaction.

### Operational Bounds

The credential provider is isolate-shared so its cache stays warm across requests; re-minting an assertion and re-running `AssumeRoleWithWebIdentity` per request would dominate latency.

The outbound STS call is bounded at 10 seconds. Without a bound, a stalled federation lets the request hang until the Cloudflare edge kills it with a non-XML `error code: NNNN` body that client SDKs cannot deserialize; with it, a stall returns a parseable S3 `ServiceUnavailable` the client can retry.

> [!NOTE]
> Sharing a credential cache across requests in an isolate is subtle on Workers: an earlier implementation guarded it with an async mutex, and awaiting a lock held across requests had the request killed at a few milliseconds with empty logs. Cache sharing must not entail cross-request awaiting.

---

## Consequences

**Benefits**

- Backend-specific client code and error mapping eliminated from the proxy.
- No long-lived cloud credentials stored anywhere in the proxy.
- Trust relationships are declarative and auditable; proxy key rotation propagates through JWKS without providers reconfiguring anything.
- Unknown authentication schemes fail closed, so a misconfiguration surfaces as a denial rather than an unsigned request to a private bucket.
- Data providers can register their own storage and keep ownership of their backend.

**Costs / Risks**

- **`object_store` compiles to `wasm32-unknown-unknown` today, but only because two of its transitive dependencies have wasm feature flags turned on explicitly.** `getrandom` needs `wasm_js`, and `ring` needs `wasm32_unknown_unknown_js` — the latter because `object_store`'s GCS credential signing calls `ring`'s `SystemRandom`. Neither crate is used by proxy code; both appear in `Cargo.toml` under `[target.'cfg(target_arch = "wasm32")'.dependencies]` purely to flip those features, so the workaround is invisible unless you read the manifest. The wasm32 `cargo check` and `cargo clippy` steps in CI are the regression guard. **Enabling an additional backend is the most likely trigger for a recurrence** — this is exactly how it surfaced the first time, when GCS was added (#191).
- Registering the proxy as a trusted IdP is a per-provider setup step.
- **Only AWS web-identity federation is implemented.** Azure and GCS connections can serve public buckets unsigned, but their workload-identity variants are denied — as are the secret-bearing `s3_access_key` and `az_sas_token` types the API also models, so an S3 connection configured with an access key is denied too. See ADR-012.
- **The shared audience puts the whole isolation boundary on provider-authored `sub` conditions**, which fails open if a provider omits or removes one.
- The `scv1:` grammar is a public contract embedded in provider-side policies; changing it breaks every configured provider, so it must be versioned and treated as an API.
- Rotating a *connection's* identity is a coordinated change with the provider, not a unilateral one.

---

## Alternatives Considered

**Manual per-backend adapters (previous model)** — rejected. Maintenance-intensive, creates ongoing integration gaps, does not scale with new backends.

**Falling back to unsigned when an authentication type is unrecognised** — rejected. Would silently expose backends that are only accidentally anonymously readable. Denying makes the misconfiguration visible.

**Encoding account or product scope in custom JWT claims** — rejected as unimplementable on AWS, which ignores all claims but `amr`, `aud`, `email`, `oaud`, and `sub` for a generic OIDC provider.

**Encoding scope as suffixes on the customer-facing `sub`** — rejected. Azure matches `subject` exactly with no wildcards, so a varying subject needs one pre-registered credential per value. It also multiplies cache cardinality and risks truncation in `RoleSessionName` (64 characters), degrading CloudTrail attribution precisely where it matters most.

**Provider-managed proxy instances** — considered. Each provider runs their own proxy with their own credentials. Rejected: fragments the platform and defeats the purpose of a unified distribution layer.

**A per-connection audience** — this is the stronger design and is *not* what shipped; see ADR-012 for the argument and the migration.

**Stored credential secrets as a fallback** — specified in the RFC for backends without OIDC federation (R2, Backblaze B2, Wasabi, DigitalOcean Spaces). Not implemented; the proxy holds no stored upstream credentials at all. See ADR-012.
