# ADR-012: Outbound Federation Coverage — Per-Connection Audience, GCP/Azure, and Stored Credentials

**Status:** Proposed — not implemented
**Date:** 2026-08-09
**RFC:** RFC-001 §9
**Depends on:** ADR-006

---

## Context

ADR-006 ships outbound federation for one cloud and one shape: AWS `AssumeRoleWithWebIdentity`, with a per-connection `sub` and the fixed audience `sts.amazonaws.com`. Three gaps remain, listed here in priority order because the first is a security property and the others are coverage.

---

## Decision

### 1. Per-Connection Audience for Direct Federation

**Today the `sub` allowlist is the only boundary between connections.** Every connection's token carries `aud: sts.amazonaws.com`, so a provider whose role trust policy omits a `sub` condition — or who removes it while debugging a failed exchange — accepts tokens minted for *any* connection on the platform. That fails open, and it fails open through the exact action a frustrated operator is most likely to take.

The fix moves the boundary to a step the provider cannot skip. Each cloud validates the token audience against the provider registration **before** any role trust policy or IAM binding is evaluated:

- **AWS** — `aud` must match a client ID registered on the IAM OIDC provider (max 100 per provider); a mismatch fails `AssumeRoleWithWebIdentity` outright
- **GCP** — `allowed_audiences` on the workload identity pool provider
- **Azure** — the federated identity credential's `issuer`, `subject`, and `audience` must all match case-sensitively

So a provider registers exactly one audience — their own connection's:

```
aws iam create-open-id-connect-provider \
  --url https://data.source.coop \
  --client-id-list scv1:conn:acme--acme-bucket
```

A token minted for any other connection carries a different audience and is rejected before policy evaluation. **The provider is protected even if their trust policy carries no conditions at all.** This makes the safe configuration the default rather than something to remember. Providers may additionally pin `sub`, and are encouraged to, but correctness no longer depends on it.

**Brokered access cannot use this mechanism.** A cloud account registers only one OIDC provider per issuer URL, so Source Cooperative's own provider must accept the audiences of every platform connection — audience validation cannot discriminate among them. Brokered roles therefore keep the `sub` allowlist from ADR-006. The two mechanisms are complementary: audience secures direct federation, the `sub` allowlist secures brokered access.

**Migration is coordinated, not unilateral.** Changing a connection's audience requires the provider to update their IdP registration; a proxy-side flip would break every configured connection at once. Sequence:

1. Add a per-connection `audience` field to the data connection model, defaulting to `sts.amazonaws.com`.
2. Register the per-connection audience alongside the existing one on each provider's IAM OIDC provider (AWS allows up to 100 client IDs, so both can be registered simultaneously).
3. Flip connections individually once their provider confirms registration.
4. Remove `sts.amazonaws.com` from provider registrations once no connection uses it.

**Re-check the cache-keying invariant in the same change.** ADR-006 requires `aud` to remain a pure function of data already in the cache key. It survives — `sub` contains the connection ID and `aud` is derived from it — but the invariant must be verified rather than assumed, and adding `aud` to the key outright is the cheaper, safer option.

### 2. GCP and Azure Federation

The Source Cooperative API already models `gcp_workload_identity` and `azure_workload_identity` connection types; the proxy maps them to `Unsupported` and denies. Azure and GCS connections can serve public buckets unsigned, but nothing private.

Each needs its own exchange:

- **GCP** — exchange the assertion at the Security Token Service for a federated token, optionally impersonating a service account
- **Azure** — present the assertion as a client assertion for a federated identity credential

Both require multistore support, which is where the AWS path lives today. Note Azure's exact-match `subject` requirement is already accommodated by ADR-006's decision to keep `sub` a stable per-connection identity.

### 3. Stored Credentials as a Fallback

RFC-001 §9 specified stored credentials for backends that cannot federate. Not implemented: the proxy holds no upstream credentials at all, and connections it cannot sign for are denied.

Backends with **no** external OIDC federation for storage access:

- **Cloudflare R2** — API tokens or access key pairs only
- **Backblaze B2** — application keys only
- **Wasabi** — STS `AssumeRole` for its own IAM users; OIDC limited to console SSO
- **DigitalOcean Spaces** — no external OIDC trust; workload identity limited to DigitalOcean's own Droplet tokens

Supporting these means the platform holds long-lived third-party secrets — the thing ADR-001 set out to eliminate on the inbound side. It should be built only against a real provider request, with per-connection isolation and a rotation story decided up front.

> [!NOTE]
> **Two backends named in RFC-001's goals are absent from the proxy's provider mapping,** though `object_store` supports both.
>
> - **R2** cannot federate — it offers API tokens or access key pairs only. It needs item 3, not item 2.
> - **HTTP** needs neither: an HTTP backend carries no credentials to federate and is served unsigned. Adding it is a provider-mapping change alone, independent of everything in this ADR — which is why it is recorded here rather than proposed here.

---

## Consequences

**Benefits**

- Provider isolation stops depending on provider-authored conditions, closing the fail-open path in ADR-006.
- Private GCS and Azure connections become usable, matching what the API already models and the RFC promised.
- Backends that cannot federate become supportable at all.

**Costs / Risks**

- The audience migration is a coordinated change across every configured provider; a mis-sequenced rollout breaks live connections.
- Per-connection audiences multiply provider-side registration entries and are bounded (AWS: 100 client IDs per provider), which constrains how many connections one provider account can hold.
- GCP and Azure federation each add an exchange protocol, an error surface, and a credential cache path.
- **Expect wasm feature-flag work.** Enabling a backend is what has historically broken the wasm32 build: GCS needed `ring`'s `wasm32_unknown_unknown_js` feature turned on for its credential signing (#191). Budget for the same shape of problem on the GCP and Azure paths, and check `cargo check --target wasm32-unknown-unknown` early rather than at review. See ADR-006 Costs / Risks.
- Stored credentials reintroduce long-lived secrets, with rotation and blast-radius problems this architecture otherwise avoids.
- The `scv1:` grammar becomes load-bearing in two claims rather than one; both are a public contract.

---

## Alternatives Considered

**Keep the shared `sts.amazonaws.com` audience and rely on `sub` conditions** — this is the status quo and the conventional choice (GitHub Actions and EKS IRSA both use it). Rejected: it makes every connection's token interchangeable at the provider-registration layer, so a provider is protected only if they author a correct condition, and unprotected the moment they remove it.

**Require every provider to pin `sub`, enforced by a documentation checklist** — rejected. A security boundary that depends on a provider following documentation is not a boundary. Nothing on the platform can verify a provider's trust policy.

**Negated conditions (`StringNotLike`) to exclude other connections** — rejected as actively unsafe. In AWS IAM a negated string condition whose context key is absent evaluates to **true**, so it is safe only when paired with a positive condition that fails closed.

**Per-request STS session policies for prefix confinement** — see ADR-011; it interacts with credential cache keying and is better expressed as a bucket policy conditioned on the session subject.

**Drop GCP/Azure support and standardise on S3-compatible backends** — considered. Simplifies the proxy, but the API already models these connection types and the RFC commits to `object_store`'s backend coverage.
