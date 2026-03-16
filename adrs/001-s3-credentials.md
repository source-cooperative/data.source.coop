# ADR-001: S3 API Compatibility and Temporary-Credentials-Only Credential Model

**Status:** Draft  
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
AccessKeyId     (e.g. "ASIA...")
SecretAccessKey (short-lived derived key)
SessionToken    (signed JWT encoding identity, role, and expiry)
```

Callers obtain these credentials by exchanging a trusted identity token at the STS endpoint (`POST /.sts/assume-role-with-web-identity`) before making S3 API calls.

### Session Token Design

The `SessionToken` is a stateless signed JWT. Its payload contains:

```json
{
  "user_id": "<stable identity identifier>",
  "role_id": "<anonymous|authenticated_user|admin>",
  "access_key_id": "<the AccessKeyId>",
  "secret_access_key": "<the SecretAccessKey for SigV4 verification>",
  "exp": "<unix timestamp>"
}
```

The proxy verifies incoming SigV4 requests by:

1. Extracting the `AccessKeyId` from the `Authorization` header
2. Looking up the corresponding `SessionToken` — presented as the `X-Amz-Security-Token` header
3. Verifying the JWT signature against the proxy's public key
4. Checking `exp` has not passed
5. Reconstructing the expected SigV4 signature using the `SecretAccessKey` from the token payload and comparing it to the presented signature

No external database lookup is required to verify a request. The token is self-contained.

**Permissions are not encoded in the session token.** The token encodes identity and role only. Per-request permission resolution is handled by the authorisation layer (see ADR-005) by consulting the policy store at request time. This is the same model AWS uses: the STS token asserts role membership, and IAM evaluates the role's current policies live on each API call.

### Accepted Trade-offs

**Tokens cannot be revoked once issued.** A compromised session token remains valid until its `exp`. Short TTLs (15–60 minutes recommended) limit the blast radius. Immediate revocation is out of scope for this iteration.

**Callers must perform a token exchange before making S3 API calls.** This is a one-time step per session. All major AWS SDKs handle STS-derived session credentials natively via the credential provider chain. Tooling that accepts `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, and `AWS_SESSION_TOKEN` environment variables works without modification.

**Documentation and CLI tooling must minimise the friction of the exchange step.** Users accustomed to copying a static key into a config file will encounter a new workflow. A `source login` CLI command and SDK credential provider helpers are planned to make the exchange step transparent.

---

## Consequences

**Benefits**

- No long-lived credentials anywhere in the system. Credentials expire automatically.
- Full compatibility with the existing S3 tooling ecosystem — no client changes required.
- The session token is stateless and self-verifying — no credential store on the hot path.
- Short-lived credentials limit blast radius of any credential compromise.
- Composable with OIDC workload identity federation (see ADR-004) — the exchange step is the same regardless of the upstream identity source.

**Costs / Risks**

- Callers must perform a token exchange before first use. This is new friction compared to the current static key model.
- The `/.sts` exchange endpoint is on the critical path for session establishment. Its availability affects whether callers can obtain credentials.
- Session tokens cannot be revoked. A credential leaked mid-session remains valid until TTL expiry.
- S3 tooling that hardcodes static credential configuration (rather than using the SDK credential provider chain) may require workarounds.

---

## Alternatives Considered

**Long-lived static credentials (current model)** — rejected. Persistent security liability; does not compose with workload identity federation; difficult to audit or rotate at scale.

**Short-lived credentials with a server-side revocation list** — considered. Would allow immediate invalidation of compromised credentials. Rejected for this iteration: adds a stateful dependency on the hot path of every request, increasing latency and operational complexity. Can be added in a future iteration if the threat model requires it.

**Custom non-S3 protocol** — rejected. Would require Source-specific client libraries and break compatibility with the entire existing ecosystem of data tooling.