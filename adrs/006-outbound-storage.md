# ADR-006: Outbound Connectivity — OIDC Issuer Model and `object_store` Adoption

**Date:** 2026-03-14
**RFC:** RFC-001 §9
**Depends on:** ADR-002
**Language:** ASD-STE100 Simplified Technical English

---

## Context

The proxy receives a request that is authenticated and authorised. The proxy must then read or write the object in an upstream storage backend such as S3, GCS, Azure Blob, or R2. This outbound connection must also be authenticated. But the proxy service must contain no long-lived cloud credentials.

The current proxy contains one manually written adapter for each cloud storage provider. Each adapter maps the errors of one client library to the internal error types. This needs much maintenance. It also causes a continuous gap when we add a new backend or when a client API changes.

Source Cooperative also intends to support **data providers** who register their own upstream storage with the platform. The proxy is then in front of their buckets and gives authentication, authorization, rate limits, and metering.

---

## Decision

### `object_store` as One Storage Abstraction

The [`object_store`](https://crates.io/crates/object_store) crate replaces each manual backend adapter. `object_store` gives one async trait (`ObjectStore`), with implementations for S3, GCS, Azure Blob, R2, HTTP, and the local file system.

Thus the proxy codebase contains no backend-specific client code and no error mapping. Each new backend of `object_store` becomes available with no change to the proxy.

### Outbound Authentication — OIDC Tokens (Preferred)

Source Cooperative operates as an OIDC identity provider. It publishes:
- `/.well-known/openid-configuration` — the OIDC discovery document
- A JWKS endpoint — the public keys that check the tokens from the proxy

An upstream cloud provider (AWS, GCP, or Azure) registers Source Cooperative as a trusted external identity provider in its own workload identity federation. The proxy then makes a short-lived JWT with a specific audience and exchanges it for cloud credentials at the STS of that provider. The section [Outbound Token Contract](#outbound-token-contract--aud-and-sub) below specifies the exact `aud` and `sub` values and gives the reasons for them. That contract is the mechanism with which an upstream cloud limits *which* data connection can use a given role.

This model gives four results:
- The proxy holds no long-lived cloud credentials.
- The credentials are temporary.
- The trust relation is declarative and auditable.
- A key rotation at the proxy applies automatically, and the upstream provider does not change its configuration.

#### Direct Federation and Brokered Role Access

A third-party data provider can give the proxy access to its storage in two ways.

1. **Direct federation** — The data provider registers Source Cooperative as a trusted OIDC identity provider in its own cloud account. The provider then makes a role, a service account, or a federated identity that the proxy can assume directly. This gives the provider full control. But the provider must configure the IdP trust in its account.

2. **Brokered role access** — Source Cooperative registers itself as an OIDC identity provider in its _own_ cloud account and assumes its own cloud role. That role is an AWS IAM role, a GCP service account, or an Azure managed identity. The data provider then gives that role cross-account access to its storage. To do this, the provider uses an S3 bucket policy, a GCS IAM binding, or an Azure role assignment. Thus the provider does not register Source Cooperative as an identity provider. The provider only trusts a cloud identity that already exists.

The brokered model is more easy for a data provider. To give a cloud role access to a bucket is a usual operation, but to register an external OIDC identity provider is not. The brokered model also keeps the OIDC configuration in one place, the cloud account of Source Cooperative. Thus each provider does not repeat that configuration. But there is a cost. The provider must trust the intermediate role of Source Cooperative, and the account of Source Cooperative becomes a choke point. An incorrect configuration or a compromise of that role has an effect on each provider that uses it.

The two models can operate together. A provider with strict security requirements, or with an existing IdP federation procedure, can use direct federation. A provider that prefers simplicity can give access to the brokered role of Source Cooperative.

#### Outbound Token Contract — `aud` and `sub`

An upstream cloud must be able to specify *which* Source Cooperative data connection can use a given role. The claims of the outbound token are the only channel for this. This section fixes that contract. It is different from the inbound contract in ADR-005, where `sub` is the `account_id`. The two contracts never meet.

**The proxy calculates both claims fully from its own server-side state. The requester has no influence on them.**

| Claim | Value | Purpose |
|---|---|---|
| `iss` | `https://data.source.coop` | Fixed. The proxy's OIDC issuer. |
| `sub` | `scv1:conn:{connection_id}` | Stable per-connection identity. Exact-matchable on every cloud. |
| `aud` | **Direct federation:** `scv1:conn:{connection_id}`<br>**Brokered:** a single platform audience | The federation boundary (see below). |

`scv1:` is a version prefix. The grammar is a public and stable contract, because customers put it in their own cloud policies. To change it breaks the configuration of each provider that uses it.

**For direct federation, the audience is the primary boundary.** Each cloud compares the audience of the token with the provider registration. It does this *before* it evaluates a role trust policy or an IAM binding:

- **AWS** — the `aud` claim must agree with one of the client IDs on the IAM OIDC provider. A provider can have a maximum of 100 client IDs. If no client ID agrees, the `AssumeRoleWithWebIdentity` call fails immediately.
- **GCP** — the `allowed_audiences` field on the workload identity pool provider.
- **Azure** — the `issuer`, `subject`, and `audience` of the federated identity credential must all agree, and the comparison is case-sensitive.

Thus a provider registers exactly one audience, which is the audience of its own connection:

```
aws iam create-open-id-connect-provider \
  --url https://data.source.coop \
  --client-id-list scv1:conn:acme--acme-bucket
```

A token for any *other* connection has a different audience. The cloud refuses that token before it evaluates a policy. **Thus the provider is safe, and a role trust policy with no conditions is sufficient.** This is deliberate. The safe configuration is the default configuration, and the provider does not remember to add it. Also, a provider who debugs a failed exchange cannot remove the boundary by accident when the provider removes a condition. A provider can also pin the `sub` claim, and we recommend this. But the correctness does not depend on it.

Do **not** use `sts.amazonaws.com` as the audience. It is only the convention of GitHub Actions and EKS IRSA. A shared audience makes the token of each connection interchangeable at the provider-registration layer. It also moves the full boundary into the conditions of the trust policy.

**Put the scope in `sub`, and not in a custom claim.** For a generic OIDC provider, AWS gives only a fixed set of condition keys: `amr`, `aud`, `email`, `oaud`, and `sub`. AWS ignores each other claim. Thus a trust policy cannot use a custom `account` or `product` claim as a condition, although that would be convenient. Each scope that an upstream policy must see must be in `sub`.

**Keep `sub` a stable per-connection identity. Do not add a variable scope to it.** Azure compares the `subject` exactly and supports no wildcard. Thus a subject that changes for each product or for each account makes the provider register one federated credential for each value. AWS and GCP accept a pattern match. But a design that operates on only two of three clouds is not portable. If a smaller boundary is truly necessary, make a *separate scoped subject on a role that Source owns* (refer to the brokered model below). Do not add the scope to the subject that the customer sees.

**The brokered model needs a different mechanism.** A cloud account can register only one OIDC provider for each issuer URL. Thus the provider of Source Cooperative must accept the audiences of *all* platform connections, and the audience check cannot distinguish between them. Therefore the trust policy of the brokered role must limit the `sub` claim directly. It uses an **allowlist** of the permitted platform subjects:

```jsonc
"StringLike": { "data.source.coop:sub": "scv1:conn:aws-opendata-*" }
```

Use an allowlist, and never a negated condition. In AWS IAM, a negated string condition evaluates to **true** when its context key is absent. Thus a `StringNotLike` condition on `sub` is safe only with a second positive condition on the same key that fails closed. A security boundary must not depend on such a fragile property. The audience check and the `sub` allowlist are complementary: the first protects direct federation, and the second protects brokered access.

#### Credential Cache Keys

The proxy caches the credentials from an exchange. The cache key must be exactly as coarse as the isolation boundary, and no more coarse. There are two invariants.

1. **Key on `(role_identifier, sub)`, and never on the role identifier alone.** With a key on the role alone, the proxy can give a connection a credential that it made for a different subject. The trust policy would refuse the subject of that connection. But the cache prevents the exchange, thus no upstream policy is used.
2. **`aud` must be a pure function of the data that is already in the key.** It is, because the proxy calculates `aud` from the connection ID, and `sub` contains that ID. If `aud` ever changes independently of `sub`, we must add `aud` to the key.

The same rule applies to any future scope for each request. **If we add an STS session policy for each request, we must add a fingerprint of that policy to the cache key in the same change.** If we do not, two requests with the same `(role, sub)` but with a different prefix collide. Then one request gets a credential with the prefix scope of the other request. That failure is a silent authorization bypass, and it defeats the mechanism that we add.

Where the cloud permits it, apply the prefix limit declaratively. On AWS, the `sub` condition key is available in the session. Thus a **bucket policy** can require that the subject of the session agree with the prefix of the request. This gives the same enforcement as a session policy. It also has no size limit, needs no calculation for each request, and has no effect on the cache key. A session policy stays available as a second layer of defence.

### Outbound Authentication — Stored Credentials (Fallback)

The current proxy gets static cloud credentials (an access key ID and a secret access key) from the Source Cooperative API for each data connection. The API keeps these credentials and gives them to the proxy on demand. The proxy caches them with a short TTL.

Some upstream providers and storage systems do not support OIDC workload identity federation. For these, this model continues: the proxy gets the stored credentials from the API and authenticates to the upstream backend with them. This is not a preferred path. A person must rotate a stored credential manually. A compromise of a stored credential does more damage. Also, the platform then holds long-lived secrets for the providers. We must encourage each data provider to configure an OIDC trust relation if its cloud supports one.

These backends **do not** support external OIDC identity federation for storage access. Therefore they need stored credentials:

- **Cloudflare R2** — only API tokens or access key pairs. There is no mechanism to trust an external OIDC issuer for storage operations.
- **Backblaze B2** — only application keys. There is no STS and no federation mechanism.
- **Wasabi** — it supports the STS `AssumeRole` operation for its own IAM users. But its OIDC integration is only for console SSO, and not for storage API federation from an external identity provider.
- **DigitalOcean Spaces** — it cannot trust an external OIDC issuer. Its workload identity operates only with the tokens that DigitalOcean issues to its own Droplets.

### Hosting for Data Providers

A data provider registers its upstream storage (its own S3 bucket, GCS bucket, or equivalent) with Source Cooperative. The proxy then operates as a layer for access control, metering, and distribution in front of that data.

A data provider gets:
- **Cost control** — rate limits, metering, and access thresholds keep the egress costs in control
- **Access control** — a precise configuration of roles and policies
- **Exposure** — users find the data through the Source Cooperative platform and UI
- **Flexible outbound authentication** — the proxy uses the cloud credentials of the provider, or an OIDC trust relation with the provider

---

## Consequences

**Benefits**

- The proxy codebase contains no backend-specific client code and no error mapping.
- Each new `object_store` backend becomes available to the proxy with no change.
- The preferred outbound model uses no long-lived credentials.
- A data provider can register its own storage and use the access control and distribution layer of Source Cooperative.

**Costs and Risks**

- `object_store` must compile to `wasm32-unknown-unknown` for the Workers target. We must avoid or patch each feature that does not operate in WASM.
- The OIDC issuer model makes each upstream cloud provider register Source Cooperative as a trusted IdP. This is a setup step for each provider.
- The stored-secret fallback puts long-lived credentials into the system again, for each provider with no OIDC federation.
- For **direct federation**, the per-connection audience keeps the providers separate. Only the provider that registered an audience accepts the token of that connection. For the **brokered** model, the separation depends on the `sub` allowlist in the trust policy of Source Cooperative, and on the prefix resolution at request time. Thus the brokered role stays the choke point above.
- The `scv1:` grammar for the subject and the audience is a public contract in the cloud policies of the providers. If we change it, the configuration of each provider fails. Therefore we must version it and treat it as an API.
- JWKS publication does the rotation of the signing keys of the proxy. But the rotation of the identity of a *connection*, which is its audience, makes the provider update its IdP registration. This is a coordinated change and not a unilateral one.

---

## Alternatives Considered

**Manual per-backend adapters (the current model)** — rejected. They need much maintenance, they cause continuous integration gaps, and they do not scale with new backends.

**One proxy instance for each provider** — considered. Each data provider then operates its own proxy instance with its own credentials. Rejected: this divides the platform, makes the access control more complex, and defeats the purpose of one distribution layer.

**All upstream credentials in a secrets manager, for example AWS Secrets Manager** — considered as the primary model and not as the fallback. Rejected in favour of OIDC. A secrets manager continues to hold long-lived credentials, and a person must rotate them. OIDC federation removes the stored secrets fully for each provider that supports it.

**One shared outbound audience, for example `sts.amazonaws.com`, with the separation only from a `sub` condition in the trust policy of each provider** — rejected. This is the conventional choice and the most easy to write. But it makes the token of each connection interchangeable at the provider-registration layer. Thus a provider is safe only with a correct condition. That fails open. A provider who does not write the condition, or who removes it during debug work, silently gives access to each connection on the platform. A per-connection audience moves the boundary to a step that the provider cannot omit.

**The account or product scope in a custom JWT claim** — rejected, because AWS cannot use it. For a generic OIDC provider, AWS gives only `amr`, `aud`, `email`, `oaud`, and `sub` as condition keys, and ignores each other claim.

**The account or product scope as a suffix on the `sub` claim that the customer sees** — rejected. Azure compares the `subject` exactly and supports no wildcard. Thus a variable subject makes the provider register one federated credential for each value. It also multiplies the number of entries in the credential cache. Also, a scope suffix can make the `RoleSessionName` field too long, because that field has a limit of 64 characters. Then the CloudTrail attribution becomes less precise, exactly where the precision is most necessary.
