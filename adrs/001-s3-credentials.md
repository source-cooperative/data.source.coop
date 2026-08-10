# ADR-001: S3 API Compatibility and Temporary-Credentials-Only Credential Model

**Status:** Accepted — implemented
**Date:** 2026-03-14
**RFC:** RFC-001 §4
**Implementation:** `src/lib.rs`, `src/config.rs`, `multistore-sts`
**Implemented by:** #116 (proxy rebuild on multistore + Workers), #165 (configurable session TTL), #176 (encoded-path SigV4 verification), #181 / #198 (multipart and CopyObject signing fixes)

---

## Context

Source Cooperative exposes a data proxy that must be consumable by the broadest possible range of data engineering tooling without requiring Source-specific client libraries. The S3 API has become the de facto standard protocol for object storage access. The ecosystem of compatible tooling is vast — all of the following speak S3 natively:

- AWS SDKs in every major language
- CLI tools (`aws s3`, `rclone`)
- Data frameworks (DuckDB, Polars, PyArrow, fsspec, GDAL/VSI)
- Orchestration systems (Airflow, Dagster, Prefect)
- Notebook environments

The current proxy implements S3 compatibility and issues long-lived static `Access Key ID` / `Secret Access Key` pairs per user. Long-lived static credentials are a persistent security liability: they are frequently stored in plaintext config files, are difficult to rotate, and have no ambient context about the caller's environment or intended scope. Several high-profile incidents in the Source Cooperative infrastructure (including a compromised IAM credential used to conduct an SES email campaign) underscore the operational risk of long-lived secrets.

The industry has broadly moved toward short-lived, exchanged credentials via OIDC workload identity federation. AWS STS, GCP Workload Identity Federation, and Azure Federated Identity Credentials all use the same underlying pattern: a trusted identity token is exchanged for short-lived scoped credentials at a Security Token Service. This pattern eliminates stored secrets on the caller side and ensures credentials expire automatically.

---

## Decision

### S3 API Compatibility

We implement the AWS Signature Version 4 (SigV4) HMAC request signing protocol. All S3-compatible clients sign requests using an `Authorization` header derived from an `Access Key ID` and `Secret Access Key`. The proxy verifies this signature on every incoming request.

This is unchanged from the current proxy. S3 API compatibility is a non-negotiable requirement for ecosystem reach.

### Temporary Credentials Only

**We do not issue or support long-lived static `Access Key ID` / `Secret Access Key` pairs.**

The proxy has no credential store to consult: its `CredentialRegistry::get_credential` returns `None` unconditionally, so there is no code path by which a static key could be honoured. All SigV4 credentials issued by Source Cooperative are temporary session credentials — the same triplet shape that AWS STS issues:

```
AccessKeyId     (random identifier, generated per session)
SecretAccessKey (random 40-character secret, generated per session)
SessionToken    (sealed token carrying the credential set and its metadata)
```

Callers obtain these credentials by exchanging a trusted identity token at the STS endpoint before making S3 API calls (see ADR-004).

### Session Token Design — Sealed Tokens

The `SessionToken` is an **AES-256-GCM sealed blob**: `base64url(nonce[12] || ciphertext+tag[16])`. The full credential set is serialised and encrypted into the token itself under a symmetric key (`SESSION_TOKEN_KEY`, a base64-encoded 32-byte key held as a Worker secret).

The sealed payload carries:

| Field | Purpose |
|---|---|
| `access_key_id` | The identifier the caller signs with |
| `secret_access_key` | The signing secret, recovered by unsealing |
| `expiration` | Enforced at unseal time; an expired token fails closed |
| `assumed_role_id` | The Role assumed at exchange time (currently always `_default`) |
| `source_identity` | The original OIDC `sub` — the caller's Ory identity |
| `allowed_scopes` | Scope ceiling sealed at mint time (currently empty — unlimited) |

Key properties of this design:

- **Verification is fully stateless.** The proxy decrypts the token on each request and recovers the `SecretAccessKey` directly. No database lookup, no key derivation, and no asymmetric verification on the request hot path — which matters on Workers, where in-memory state does not persist across invocations.
- **The token is opaque to the caller.** Unlike a JWT, a client cannot read the sealed payload. Scope and identity metadata are not disclosed to whoever holds the credential.
- **Scope is bound at mint time.** `allowed_scopes` is sealed when the credential is issued, so later configuration changes affect only newly minted credentials, never ones already in flight.
- **`source_identity` preserves the original subject**, which is what the proxy presents to the policy store (see ADR-005).
- **Authenticated encryption.** GCM provides integrity as well as confidentiality: a tampered token fails to decrypt rather than decoding into attacker-chosen values.

### SigV4 Verification Flow

1. Extract the `AccessKeyId` from the `Authorization` header
2. Unseal the `SessionToken` from the `X-Amz-Security-Token` header, recovering the credential set
3. Reject if the sealed `expiration` has passed
4. Verify the SigV4 signature using the unsealed `SecretAccessKey`, over the percent-encoded request path as signed by the client
5. Proceed to authorization (see ADR-005) using the token's `source_identity`

### Session Lifetime

Client-requested `DurationSeconds` is clamped to `[900, STS_MAX_SESSION_DURATION_SECS]`. Production sets the ceiling to 43200 (12 hours); unset defaults to 3600 (1 hour). This matches the RFC's 15-minute-to-12-hour window.

### Key Management and Revocation

- **`SESSION_TOKEN_KEY`** is a symmetric AES-256 key, held as a Worker secret and re-uploaded by CI on every deploy.
- **Rotation invalidates all sessions.** A token sealed under an old key fails to decrypt and the client re-authenticates. This is the incident-response lever: there is no per-token revocation.
- Short credential lifetimes (15 minutes to 12 hours) bound the exposure window of a leaked token.

> [!NOTE]
> **Deferred: per-token revocation.** Not implemented, and not straightforward under sealed tokens — there is no `jti` to deny-list without adding one to the sealed payload and consulting a store on every request, which would give up the statelessness this design is built on. The available response to a compromised credential is rotating `SESSION_TOKEN_KEY`, which signs out every active session.

> [!NOTE]
> **Deferred: long-lived API keys.** The proxy accepts only STS-issued session credentials and anonymous access. Environments with neither ambient OIDC tokens nor browser access — HPC clusters, on-premises instruments, legacy ETL — are not served. See ADR-013, which keeps the single authorization path by exchanging an API key at `/.sts` like any other token.

> [!NOTE]
> **Deferred: signing-key versioning in the AccessKeyId.** The RFC reserved a version indicator in an `SCSTS`-prefixed AccessKeyId to support rotating credential keys without invalidating live sessions. The shipped AccessKeyId is an opaque random identifier with no embedded version, so staged rotation is not available; see the rotation note above.

---

## Consequences

**Benefits**

- No long-lived credentials anywhere in the system. Credentials expire automatically.
- Full compatibility with the existing S3 tooling ecosystem — no client changes required.
- The session token is self-contained — no credential store on the hot path, and no per-request asymmetric cryptography.
- The token is opaque: intercepting it reveals no identity or scope metadata.
- Composable with OIDC workload identity federation (see ADR-004) — the exchange step is the same regardless of the upstream identity source.

**Costs / Risks**

- Callers must perform a token exchange before first use. This is new friction compared to the current static key model.
- The `/.sts` exchange endpoint is on the critical path for session establishment. Its availability affects whether callers can obtain credentials.
- **`SESSION_TOKEN_KEY` is a single symmetric secret whose compromise affects every active session**, and which is necessarily present in the same environment that verifies requests. There is no asymmetric split between an issuing key and a verifying key.
- No per-token revocation, and no staged key rotation — the only lever invalidates every session at once.
- The sealed token carries the `SecretAccessKey` itself, so a leaked SessionToken plus its AccessKeyId is a complete credential set until it expires. (The RFC's HMAC-derivation design was chosen partly to avoid this; see Alternatives.)
- S3 tooling that hardcodes static credential configuration (rather than using the SDK credential provider chain) may require workarounds.

---

## Alternatives Considered

**Long-lived static credentials (current model)** — rejected. Persistent security liability; does not compose with workload identity federation; difficult to audit or rotate at scale.

**ES256-signed JWT session tokens with HMAC-derived secrets** — this was the original RFC-001 §4 proposal and is not what shipped. The design was: a `SessionToken` signed with ES256 (ECDSA P-256) carrying `account_id`, `role_name`, `permissions`, `assumed_by` and `kid`, with `SecretAccessKey = HMAC-SHA256(server_secret, AccessKeyId)` derived per request rather than stored in the token, and an `SCSTS{version}` AccessKeyId prefix enabling staged HMAC key rotation.

It was not implemented, for two reasons. First, `multistore-sts` supplies sealed tokens as its credential primitive, and adopting them kept the Source-specific code to a registry implementation rather than a parallel token stack. Second, the JWT design's main advantages — an embedded permission ceiling and an `assumed_by` audit claim — only pay off once Roles exist (ADR-010) and authorization reads a ceiling from the token (ADR-011); with a single unlimited `_default` Role, both claims would be constants.

Its advantages remain real and unrealised: the `SecretAccessKey` never travelling inside the token, an asymmetric split between issuing and verifying keys, and `kid`-based rotation that does not sign out every session. **Revisit when ADR-010 and ADR-011 are implemented** — that is the point at which the token needs to carry a ceiling and an audit subject anyway, and the token format should be reconsidered as part of that work rather than separately.

**Server-side session store for SecretAccessKey** — considered. Generating a random SecretAccessKey per session and storing it server-side eliminates any shared-key risk. Rejected: adds a mandatory store read on every request for credential verification. Sealed tokens keep verification stateless without that lookup.

**Symmetric signing (HS256)** — rejected as a JWT variant. Would require the signing secret on all verification endpoints. Note that the shipped sealed-token design has the same property — a single symmetric secret — and accepts it for the statelessness gain.

**Custom non-S3 protocol** — rejected. Would require Source-specific client libraries and break compatibility with the entire existing ecosystem of data tooling.
