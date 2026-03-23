# ADR-001: S3 API Compatibility and Temporary-Credentials-Only Credential Model

**Status:** Draft
**Date:** 2026-03-14
**Updated:** 2026-03-22
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
AccessKeyId     (e.g. "SCSTS...")
SecretAccessKey (HMAC-derived key)
SessionToken    (signed JWT encoding identity, role, permissions, and expiry)
```

Callers obtain these credentials by exchanging a trusted identity token at the STS endpoint (`POST /.sts/assume-role-with-web-identity`) before making S3 API calls. The `AccessKeyId` is prefixed with `SCSTS` to distinguish STS-issued credentials from any legacy permanent keys during the migration period.

### Session Token Design

The `SessionToken` is a signed JWT using ES256 (ECDSA P-256) asymmetric signing. Its payload contains:

```json
{
  "jti": "<unique token ID for revocation>",
  "sub": "source::my-org::role/github-publisher",
  "account_id": "my-org",
  "role_name": "github-publisher",
  "assumed_by": "repo:my-org/my-repo:ref:refs/heads/main",
  "assumed_by_issuer": "https://token.actions.githubusercontent.com",
  "session_name": "my-ci-job-42",
  "access_key_id": "SCSTS...",
  "permissions": [
    {"actions": ["read", "write"], "resources": ["source::my-org::product/climate-data/*"]}
  ],
  "iat": 1711100000,
  "exp": 1711103600,
  "aud": "data.source.coop",
  "kid": "<signing key ID>"
}
```

Key properties of this design:

- **The `SecretAccessKey` is not in the token.** It is derived deterministically on each request: `SecretAccessKey = HMAC-SHA256(server_secret, AccessKeyId)`. The server reconstructs it by re-deriving from the `AccessKeyId`. This prevents a leaked SessionToken from directly yielding a complete credential set.
- **`assumed_by` and `assumed_by_issuer`** preserve the original IdP subject for audit trails, even though the credentials act on behalf of the account.
- **`permissions`** embed the Role's permission ceiling in the token, avoiding a per-request policy store lookup for Role evaluation. The account's underlying permissions are still resolved dynamically (see ADR-005).
- **`jti`** enables lightweight revocation via a deny-list (see below).
- **`kid`** in the JWT header supports signing key rotation.

### SigV4 Verification Flow

The proxy verifies incoming SigV4 requests by:

1. Extracting the `AccessKeyId` from the `Authorization` header
2. Detecting the `SCSTS` prefix to identify this as an STS credential
3. Deriving the `SecretAccessKey` via `HMAC-SHA256(server_secret, AccessKeyId)`
4. Verifying the SigV4 signature using the derived secret
5. Extracting and verifying the `SessionToken` JWT from the `X-Amz-Security-Token` header — checking ES256 signature, `exp`, `aud`, and `jti` against the revocation deny-list
6. Proceeding to authorization (see ADR-005) using the token's embedded identity and permissions

No external database lookup is required to verify a request or reconstruct the signing key. The token and HMAC derivation together are self-contained.

### Signing Key Management

- **Asymmetric signing:** ES256 (ECDSA P-256). The private key is used only for token issuance; the public key is served at a JWKS endpoint for verification.
- **Key storage:** Private key stored in KMS (AWS KMS or equivalent).
- **Key rotation:** The `kid` header in issued JWTs allows multiple active signing keys. During rotation, new tokens are signed with the new key while tokens signed with the old key remain valid until they expire. The old key is retired after one `max_session_duration` interval.
- **HMAC server secret:** A separate symmetric key used for SecretAccessKey derivation. Stored alongside the signing key in KMS. Rotation follows the same pattern — new AccessKeyIds use the new secret; existing sessions continue to work until expiry.

### Revocation

Session tokens support lightweight revocation via a deny-list of `jti` values:

- Revoked `jti` values are stored in Cloudflare KV (or equivalent) with a TTL matching the token's remaining lifetime. Entries self-clean as tokens expire.
- Every authenticated request checks the deny-list (~1ms KV lookup at edge).
- Account admins can revoke tokens via `POST /.sts/revoke-session-token`.

This is a targeted mechanism for incident response, not a general session management system. The deny-list stays small because entries expire automatically.

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
- Revocation is supported via a lightweight deny-list without adding a stateful dependency to the verification hot path.
- Short-lived credentials (15 min to 12 hours) further limit blast radius.
- Composable with OIDC workload identity federation (see ADR-004) — the exchange step is the same regardless of the upstream identity source.

**Costs / Risks**

- Callers must perform a token exchange before first use. This is new friction compared to the current static key model.
- The `/.sts` exchange endpoint is on the critical path for session establishment. Its availability affects whether callers can obtain credentials.
- The HMAC server secret is a high-value target. Its compromise, combined with a captured SessionToken, yields the corresponding SecretAccessKey.
- The revocation deny-list introduces a KV dependency on the request path, though the lookup is fast (~1ms) and the system degrades gracefully if KV is unavailable (tokens remain valid until expiry).
- S3 tooling that hardcodes static credential configuration (rather than using the SDK credential provider chain) may require workarounds.

---

## Alternatives Considered

**Long-lived static credentials (current model)** — rejected. Persistent security liability; does not compose with workload identity federation; difficult to audit or rotate at scale.

**Embedding the SecretAccessKey in the SessionToken** — rejected. A leaked SessionToken would contain a complete, self-sufficient credential set. HMAC derivation separates the signing material from the bearer token, requiring compromise of both the server secret and a token to reconstruct credentials.

**Server-side session store for SecretAccessKey** — considered. Generating a random SecretAccessKey per session and storing it in KV eliminates the HMAC shared secret risk entirely. Rejected for now: adds a mandatory KV read on every request for credential verification (not just revocation checks). The HMAC approach keeps verification fully stateless. Can be revisited if the threat model changes.

**Symmetric signing (HS256)** — rejected. Would require the signing secret to be available on all verification endpoints, expanding the attack surface. ES256 limits the private key to the issuance path only.

**Custom non-S3 protocol** — rejected. Would require Source-specific client libraries and break compatibility with the entire existing ecosystem of data tooling.
