# ADR-001: S3 API Compatibility and Temporary-Credentials-Only Credential Model

**Date:** 2026-03-14
**RFC:** RFC-001 §4

---

## Context

Source Cooperative exposes a data proxy that must be consumable by the broadest possible range of data engineering tooling without requiring Source-specific client libraries. The S3 API has become the de facto standard protocol for object storage access. The ecosystem of compatible tooling is vast: AWS SDKs in every major language, CLI tools (`aws s3`, `rclone`), data frameworks (DuckDB, Polars, PyArrow, fsspec, GDAL/VSI), orchestration systems (Airflow, Dagster, Prefect), and notebook environments all speak S3 natively.

The current proxy implements S3 compatibility and issues long-lived static `Access Key ID` / `Secret Access Key` pairs per user. Long-lived static credentials are a persistent security liability: they are frequently stored in plaintext config files, are difficult to rotate, and have no ambient context about the caller's environment or intended scope. Several high-profile incidents in the Source Cooperative infrastructure (including a compromised IAM credential used to conduct an SES email campaign) underscore the operational risk of long-lived secrets.

The industry has broadly moved toward short-lived, exchanged credentials via OIDC workload identity federation. AWS STS, GCP Workload Identity Federation, and Azure Federated Identity Credentials all use the same underlying pattern: a trusted identity token is exchanged for short-lived scoped credentials at a Security Token Service. This pattern eliminates stored secrets on the caller side and ensures credentials expire automatically.

---

## Decision

### S3 API Compatibility

We implement the AWS Signature Version 4 (SigV4) HMAC request signing protocol. All S3-compatible clients sign requests using an `Authorization` header derived from an `Access Key ID` and `Secret Access Key`. The proxy verifies this signature on every incoming request.

This is unchanged from the current proxy. S3 API compatibility is a non-negotiable requirement for ecosystem reach.

### Temporary Credentials Only

**We do not issue or support long-lived static `Access Key ID` / `Secret Access Key` pairs.**

All SigV4 credentials issued by Source Cooperative are temporary session credentials — the same triplet shape that AWS STS issues:

```
AccessKeyId     (e.g. "SCSTS1...")
SecretAccessKey (HMAC-derived key)
SessionToken    (signed JWT encoding identity, role, permissions, and expiry)
```

Callers obtain these credentials by exchanging a trusted identity token at the STS endpoint (`POST /.sts/assume-role-with-web-identity`) before making S3 API calls. The `AccessKeyId` is prefixed with `SCSTS` to identify STS-issued credentials and reserve namespace for future credential types (see [Permanent API Keys](#permanent-api-keys)).

### Session Token Design

The `SessionToken` is a signed JWT using ES256 (ECDSA P-256) asymmetric signing. Its payload contains:

```json
{
  "sub": "sc::my-org::role/github-publisher",
  "account_id": "my-org",
  "role_name": "github-publisher",
  "assumed_by": "repo:my-org/my-repo:ref:refs/heads/main",
  "assumed_by_issuer": "https://token.actions.githubusercontent.com",
  "session_name": "my-ci-job-42",
  "access_key_id": "SCSTS1...",
  "permissions": [
    {"actions": ["read", "write"], "resources": ["sc::my-org::product/climate-data/*"]}
  ],
  "iat": 1711100000,
  "nbf": 1711100000,
  "exp": 1711103600,
  "aud": "data.source.coop",
  "kid": "<signing key ID>"
}
```

Key properties of this design:

- **The `SecretAccessKey` is not in the token.** It is derived deterministically on each request: `SecretAccessKey = HMAC-SHA256(server_secret, AccessKeyId)`. The server reconstructs it by re-deriving from the `AccessKeyId`. This prevents a leaked SessionToken from directly yielding a complete credential set.
- **`assumed_by` and `assumed_by_issuer`** preserve the original IdP subject for audit trails, even though the credentials act on behalf of the account.
- **`permissions`** embed the Role's permission ceiling in the token, avoiding a per-request policy store lookup for Role evaluation. The account's underlying permissions are still resolved dynamically (see ADR-005).
- **`nbf`** (not-before) prevents token use before the issued time. Set equal to `iat` at issuance; the verifier applies a 60-second clock skew tolerance.
- **`permissions`** are readable by anyone who intercepts the SessionToken. This is acceptable: the permission ceiling reveals the Role's scope but does not grant access without the corresponding SecretAccessKey (which requires the server secret to derive).
- **`kid`** in the JWT header supports signing key rotation.

### SigV4 Verification Flow

The proxy verifies incoming SigV4 requests by:

1. Extracting the `AccessKeyId` from the `Authorization` header
2. Detecting the `SCSTS` prefix to identify this as an STS credential. The digit following `SCSTS` is the HMAC key version (e.g., `SCSTS1...` uses key version 1), enabling key rotation without invalidating active sessions
3. Deriving the `SecretAccessKey` via `HMAC-SHA256(server_secret[version], AccessKeyId)`
4. Verifying the SigV4 signature using the derived secret
5. Extracting and verifying the `SessionToken` JWT from the `X-Amz-Security-Token` header — checking ES256 signature, `exp`, `nbf` (with 60s clock skew tolerance), and `aud`
6. Proceeding to authorization (see ADR-005) using the token's embedded identity and permissions

No external database lookup is required to verify a request or reconstruct the signing key. The token and HMAC derivation together are self-contained.

### Signing Key Management

- **Asymmetric signing:** ES256 (ECDSA P-256). The private key is used only for token issuance; the public key is served at a JWKS endpoint for verification.
- **Key storage:** Private key stored in KMS (AWS KMS or equivalent).
- **Key rotation:** The `kid` header in issued JWTs allows multiple active signing keys. During rotation, new tokens are signed with the new key while tokens signed with the old key remain valid until they expire. The old key is retired after one `max_session_duration` interval.
- **HMAC server secret:** A separate symmetric key used for SecretAccessKey derivation. Stored alongside the signing key in KMS. The initial implementation uses a single HMAC key version (`SCSTS1`). The version indicator in the AccessKeyId prefix is reserved for future key rotation support.

> [!NOTE]
> **Future extension: HMAC key rotation.** The `SCSTS1` prefix embeds a key version indicator. When rotation is needed, the proxy can be updated to support multiple active key versions (e.g., `SCSTS1` → `SCSTS2`): new sessions are issued with the new version, the proxy derives the SecretAccessKey using the version indicated by the prefix, and the old key is retired after one `max_session_duration` interval beyond the last issuance. For incident response before rotation is implemented, replacing the single HMAC server secret invalidates all active sessions.

### Revocation

> [!NOTE]
> **Deferred.** Per-token revocation (via a `jti` deny-list checked on every request) is not included in the initial implementation. Short-lived credentials (15 min to 12 hours) bound the exposure window of a compromised token. For incident response, rotating the HMAC server secret or the JWT signing key invalidates all active sessions.
>
> Per-token revocation can be added later by: (1) adding a `jti` claim to the SessionToken, (2) storing revoked `jti` values in Cloudflare KV with TTLs matching remaining token lifetime, and (3) checking the deny-list on each authenticated request. This is a backwards-compatible addition — existing tokens without `jti` are simply not revocable.

### Accepted Trade-offs

**HMAC derivation creates a shared secret dependency.** If the `server_secret` leaks, an attacker who also captures a SessionToken could derive the corresponding SecretAccessKey. This risk is bounded: the attacker needs both the server secret and a valid SessionToken (which requires the separate ES256 signing key to forge). The two secrets are independent.

**Callers must perform a token exchange before making S3 API calls.** This is a one-time step per session. The existing `source-coop` CLI supports `credential_process` integration, making the exchange transparent for tools that use the AWS credential provider chain.

**Documentation and CLI tooling must minimise the friction of the exchange step.** Users accustomed to copying a static key into a config file will encounter a new workflow. The `source-coop creds --role-arn <role>` command and GitHub Action handle this for the primary use cases.

---

## Consequences

**Benefits**

- No long-lived credentials anywhere in the system. Credentials expire automatically.
- Full compatibility with the existing S3 tooling ecosystem — no client changes required.
- The session token is stateless and self-verifying — no credential store on the hot path.
- The SecretAccessKey never appears in the SessionToken, limiting the blast radius of token leakage.
- Asymmetric signing (ES256) means verification requires only the public key; the private signing key has a minimal attack surface.
- Short-lived credentials (15 min to 12 hours) limit blast radius, eliminating the need for per-token revocation in the initial implementation.
- Composable with OIDC workload identity federation (see ADR-004) — the exchange step is the same regardless of the upstream identity source.

**Costs / Risks**

- Callers must perform a token exchange before first use. This is new friction compared to the current static key model.
- The `/.sts` exchange endpoint is on the critical path for session establishment. Its availability affects whether callers can obtain credentials.
- The HMAC server secret is a high-value target. Its compromise, combined with a captured SessionToken, yields the corresponding SecretAccessKey.
- No per-token revocation in the initial implementation. The only incident response option is rotating the server-wide HMAC secret or JWT signing key, which invalidates all active sessions. Per-token revocation can be added later (see [Revocation](#revocation)).
- S3 tooling that hardcodes static credential configuration (rather than using the SDK credential provider chain) may require workarounds.

---

## Permanent API Keys

> [!NOTE]
> **Not included in the initial implementation.** The proxy supports only STS-issued session credentials and anonymous access. Long-lived API keys may be added in the future for workflows where neither workload identity federation nor interactive authentication via `auth.source.coop` is feasible — for example, on-premises instruments, legacy ETL systems, or environments without OIDC support.
>
> API keys would be exchanged for temporary STS credentials at the `/.sts` endpoint — the same way OIDC tokens are exchanged today. This keeps the proxy's request-time verification uniform: only short-lived STS credentials are accepted on S3 API calls. No second authorization path is needed.

---

## Alternatives Considered

**Long-lived static credentials (current model)** — rejected. Persistent security liability; does not compose with workload identity federation; difficult to audit or rotate at scale.

**Server-side session store for SecretAccessKey** — considered. Generating a random SecretAccessKey per session and storing it in a server-side store (KV or database) eliminates the HMAC shared secret risk entirely — there is no single key whose compromise affects all sessions. Rejected for now: adds a mandatory store read on every request for credential verification. The HMAC approach keeps verification fully stateless — the server derives the SecretAccessKey from the AccessKeyId without any external lookup. Can be revisited if the threat model changes or if a per-request store dependency is introduced for other reasons.

**Symmetric signing (HS256)** — rejected. Would require the signing secret to be available on all verification endpoints, expanding the attack surface. ES256 limits the private key to the issuance path only.

**Custom non-S3 protocol** — rejected. Would require Source-specific client libraries and break compatibility with the entire existing ecosystem of data tooling.
