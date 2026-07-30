# ADR-001: S3 API Compatibility and Temporary-Credentials-Only Credential Model

**Date:** 2026-03-14
**RFC:** RFC-001 §4
**Language:** ASD-STE100 Simplified Technical English

---

## Context

Source Cooperative gives a data proxy that as many data engineering tools as possible must be able to use. These tools must not need a client library that is specific to Source Cooperative. The S3 API is now the standard protocol for access to object storage. Many tools speak S3:

- The AWS SDKs, in each important language
- CLI tools: `aws s3` and `rclone`
- Data frameworks: DuckDB, Polars, PyArrow, fsspec, and GDAL/VSI
- Orchestration systems: Airflow, Dagster, and Prefect
- Notebook environments

The current proxy is S3-compatible and gives one long-lived static `Access Key ID` and `Secret Access Key` pair to each user. Long-lived static credentials are a continuous security risk. Users frequently keep them in plaintext configuration files. It is difficult to rotate them. They also contain no data about the environment of the caller or about the intended scope. The Source Cooperative infrastructure had incidents that show this risk. In one incident, an attacker used a compromised IAM credential to send an SES email campaign.

The industry now uses short-lived credentials from OIDC workload identity federation. AWS STS, GCP Workload Identity Federation, and Azure Federated Identity Credentials all use the same pattern. A Security Token Service receives a trusted identity token and returns short-lived scoped credentials. Thus the caller keeps no secret, and each credential expires automatically.

---

## Decision

### S3 API Compatibility

We use the AWS Signature Version 4 (SigV4) HMAC protocol to sign requests. Each S3-compatible client signs its request with an `Authorization` header, which it calculates from an `Access Key ID` and a `Secret Access Key`. The proxy checks this signature on each incoming request.

This is the same as in the current proxy. S3 API compatibility is a mandatory requirement, because it gives access to the full ecosystem.

### Temporary Credentials Only

**We do not issue or support long-lived static `Access Key ID` and `Secret Access Key` pairs.**

Each SigV4 credential from Source Cooperative is a temporary session credential. It has the same shape as a credential from AWS STS:

```
AccessKeyId     (e.g. "SCSTS1...")
SecretAccessKey (HMAC-derived key)
SessionToken    (signed JWT encoding identity, role, permissions, and expiry)
```

A caller gets these credentials at the STS endpoint (`POST /.sts/assume-role-with-web-identity`). The caller gives a trusted identity token and gets the credentials in exchange. The caller does this before it sends S3 API calls. The `AccessKeyId` starts with `SCSTS`. This prefix identifies an STS credential and keeps the namespace free for other credential types (refer to [Permanent API Keys](#permanent-api-keys)).

### Design of the Session Token

The `SessionToken` is a JWT with an ES256 signature (ECDSA P-256). Its payload contains these fields:

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

This design has these primary properties:

- **The token does not contain the `SecretAccessKey`.** The server calculates it again for each request: `SecretAccessKey = HMAC-SHA256(server_secret, AccessKeyId)`. Thus a leaked SessionToken alone does not give a complete credential set.
- **`assumed_by` and `assumed_by_issuer`** keep the original IdP subject for the audit trail. The credentials operate for the account, but the audit trail shows the original identity.
- **`permissions`** contains the permission ceiling of the Role. Thus the proxy does not look in the policy store to evaluate the Role. The proxy continues to get the underlying permissions of the account dynamically (refer to ADR-005).
- **`nbf`** (not before) prevents the use of the token before its issue time. The proxy sets `nbf` equal to `iat` at the issue, and permits a clock difference of 60 seconds.
- **`permissions`** is readable by any person who intercepts the SessionToken. This is satisfactory. The permission ceiling shows the scope of the Role, but it gives no access without the related SecretAccessKey. To calculate that key, you need the server secret.
- **`kid`** is in the JWT header and makes the rotation of the signing key possible.

### SigV4 Verification Flow

The proxy checks each incoming SigV4 request in these steps:

1. Read the `AccessKeyId` from the `Authorization` header.
2. Find the `SCSTS` prefix, which shows that this is an STS credential. The digit after `SCSTS` is the version of the HMAC key. For example, `SCSTS1...` uses key version 1. Thus we can rotate the key and keep the active sessions valid.
3. Calculate the `SecretAccessKey` with `HMAC-SHA256(server_secret[version], AccessKeyId)`.
4. Check the SigV4 signature with the calculated secret.
5. Read the `SessionToken` JWT from the `X-Amz-Security-Token` header. Then check the ES256 signature, `exp`, `nbf` (with a clock difference of 60 seconds), and `aud`.
6. Continue to the authorization (refer to ADR-005) with the identity and the permissions from the token.

The proxy uses no external database to check a request or to calculate the signing key. The token and the HMAC calculation together contain all of the necessary data.

### Management of the Signing Keys

- **Asymmetric signature:** ES256 (ECDSA P-256). The proxy uses the private key only to issue a token. A JWKS endpoint gives the public key for verification.
- **Key storage:** KMS holds the private key (AWS KMS or an equivalent service).
- **Key rotation:** The `kid` header in each JWT permits more than one active signing key. During a rotation, the proxy signs each new token with the new key. Each token with the old key stays valid until it expires. We then retire the old key after one `max_session_duration` interval.
- **HMAC server secret:** This is a separate symmetric key, and the proxy uses it to calculate the SecretAccessKey. KMS holds it with the signing key. The first implementation uses one HMAC key version (`SCSTS1`). The version indicator in the AccessKeyId prefix is available for a future rotation.

> [!NOTE]
> **Future extension: HMAC key rotation.** The `SCSTS1` prefix contains a key version indicator. When a rotation becomes necessary, we can add support for more than one active key version, for example `SCSTS1` and `SCSTS2`. The proxy then issues each new session with the new version. It calculates the SecretAccessKey with the version from the prefix. It then retires the old key one `max_session_duration` interval after the last issue. Before we add this rotation, incident response is possible: if we replace the single HMAC server secret, all active sessions become invalid.

### Revocation

> [!NOTE]
> **Deferred.** The first implementation contains no revocation of a single token. Revocation of a single token needs a `jti` deny-list, and the proxy must read that list on each request. Short-lived credentials (15 minutes to 12 hours) limit the exposure of a compromised token. For incident response, we can rotate the HMAC server secret or the JWT signing key. Then all active sessions become invalid.
>
> We can add revocation of a single token later, in three steps:
>
> 1. Add a `jti` claim to the SessionToken.
> 2. Keep each revoked `jti` in Cloudflare KV. Give it a TTL equal to the remaining life of the token.
> 3. Read the deny-list on each authenticated request.
>
> This addition is backward-compatible. An existing token has no `jti`, thus we cannot revoke it.

### Accepted Trade-offs

**The HMAC calculation makes a shared-secret dependency.** If the `server_secret` leaks, an attacker who also captures a SessionToken can calculate the related SecretAccessKey. This risk is limited. The attacker needs the server secret and a valid SessionToken. To make a valid SessionToken, the attacker needs the separate ES256 signing key. The two secrets are independent.

**A caller must do a token exchange before it sends S3 API calls.** This is one step for each session. The existing `source-coop` CLI supports `credential_process`. Thus the exchange is invisible to each tool that uses the AWS credential provider chain.

**The documentation and the CLI tools must keep the exchange step easy.** Users who usually copy a static key into a configuration file get a new procedure. The command `source-coop creds --role-arn <role>` and the GitHub Action do this step for the primary use cases.

---

## Consequences

**Benefits**

- There are no long-lived credentials in the system. Each credential expires automatically.
- The existing S3 tools are fully compatible. No client changes are necessary.
- The session token is stateless and contains its own proof. There is no credential store on the hot path.
- The SessionToken does not contain the SecretAccessKey. Thus a leaked token does less damage.
- The signature is asymmetric (ES256). Verification needs only the public key, thus the private key has a small attack surface.
- Short-lived credentials (15 minutes to 12 hours) limit the damage. Thus the first implementation does not need revocation of a single token.
- The model is compatible with OIDC workload identity federation (refer to ADR-004). The exchange step is the same for each upstream identity source.

**Costs and Risks**

- Each caller must do a token exchange before the first use. This is more work than the current static key model.
- The `/.sts` endpoint is on the critical path to start a session. If it is not available, a caller cannot get credentials.
- The HMAC server secret is a high-value target. An attacker with that secret and a captured SessionToken can calculate the related SecretAccessKey.
- The first implementation has no revocation of a single token. The only incident response is to rotate the HMAC secret or the JWT signing key of the server, and this makes all active sessions invalid. We can add revocation of a single token later (refer to [Revocation](#revocation)).
- Some S3 tools contain a static credential configuration and do not use the credential provider chain of the SDK. These tools can need a workaround.

---

## Permanent API Keys

> [!NOTE]
> **Not in the first implementation.** The proxy supports only STS session credentials and anonymous access. ADR-008 gives the design of the API keys. An API key is a long-lived JWT with a signature from the OIDC issuer of the proxy. A caller exchanges it at `/.sts` for short-lived STS credentials, in the same way as any other token. API keys serve environments with no ambient OIDC token and no browser access, such as university HPC clusters, on-premises instruments, and legacy ETL systems.

---

## Alternatives Considered

**Long-lived static credentials (the current model)** — rejected. They are a continuous security risk. They are not compatible with workload identity federation. It is difficult to audit them or to rotate many of them.

**A server-side session store for the SecretAccessKey** — considered. The server makes a random SecretAccessKey for each session and keeps it in a store (KV or a database). This removes the risk of the shared HMAC secret completely, because no single key controls all sessions. We rejected it for now, because it adds a mandatory store read to each request. The HMAC approach keeps the verification fully stateless: the server calculates the SecretAccessKey from the AccessKeyId and uses no external lookup. We can examine this alternative again if the threat model changes, or if a store dependency for each request becomes necessary for a different reason.

**A symmetric signature (HS256)** — rejected. The signing secret must then be available at each verification endpoint, and this makes the attack surface larger. ES256 keeps the private key on the issue path only.

**A custom protocol that is not S3** — rejected. It needs a client library that is specific to Source Cooperative, and it breaks the compatibility with the full ecosystem of data tools.
