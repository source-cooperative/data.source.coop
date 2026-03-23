# STS Token Exchange Design

## Problem

Source Cooperative needs federated identity and fine-grained access control for its S3-compatible data proxy. Users and automated systems (CI/CD pipelines, data workflows) must obtain temporary credentials scoped to specific permissions without long-lived API keys.

## Core Entities

### Identity Providers (IdPs)

IdPs exist at two tiers:

**Platform IdPs** are pre-configured by Source Cooperative operators:
- `auth.source.coop` (Source Cooperative's Ory-based OIDC)
- `https://token.actions.githubusercontent.com` (GitHub Actions)
- `https://gitlab.com` (GitLab CI)
- Additional issuers added by operators over time

Platform IdPs define:
```json
{
  "id": "github-actions",
  "issuer_url": "https://token.actions.githubusercontent.com",
  "display_name": "GitHub Actions",
  "well_known_claims": ["repository", "repository_owner", "ref", "environment", "job_workflow_ref"],
  "audience_hint": "https://data.source.coop"
}
```

The `well_known_claims` field provides documentation and UI hints for users creating Role bindings. The `audience_hint` is the recommended `aud` value callers should configure when requesting OIDC tokens.

**Account IdPs** are registered by account owners (Individual or Organization):
- Must use HTTPS
- Issuer URL must not collide with any platform IdP (exact match after canonicalization)
- Must not duplicate another IdP on the same account
- Must serve a valid OIDC discovery document at `/.well-known/openid-configuration`
- Resolved IP must not be private, loopback, or link-local (SSRF protection)
- Fetch timeout: 3 seconds, response body limit: 256KB

Account IdP stored record:
```json
{
  "id": "uuid",
  "account_id": "my-org",
  "issuer_url": "https://corp.okta.com/oauth2/default",
  "display_name": "Our Corporate Okta",
  "created_at": "2025-03-22T...",
  "created_by": "user-id"
}
```

No JWKS is stored at registration time. JWKS is fetched and cached at STS exchange time from the OIDC discovery document's `jwks_uri`.

**IdP deletion** is blocked if any Role references the IdP. The account must first remove the IdP binding from all Roles, then delete the IdP.

### Roles

Roles belong to an account (Individual or Organization), identified by URN: `source::{account_id}::role/{role_name}`.

Each Role contains:
- **Identity constraints** — which IdPs can assume this Role, with what claim requirements
- **Permission statements** — what the Role's credentials can access
- **`max_session_duration`** — ceiling on credential TTL (default 1 hour, max 12 hours)

Role schema:
```json
{
  "name": "github-publisher",
  "display_name": "GitHub CI Publisher",
  "max_session_duration": 3600,
  "identity_constraints": [
    {
      "idp": "github-actions",
      "audience": "https://data.source.coop",
      "claim_constraints": [
        {"claim": "repository", "operator": "equals", "value": "my-org/my-repo"},
        {"claim": "ref", "operator": "starts_with", "value": "refs/heads/"}
      ]
    },
    {
      "idp": "uuid-of-account-idp",
      "audience": "https://data.source.coop",
      "claim_constraints": [
        {"claim": "sub", "operator": "equals", "value": "service-account-42"}
      ]
    }
  ],
  "permissions": [
    {
      "actions": ["read", "write"],
      "resources": ["source::my-org::product/climate-data/*"]
    },
    {
      "actions": ["read"],
      "resources": ["source::my-org::product/reference-data/*"]
    }
  ]
}
```

**Built-in default Role:** `source::{account_id}::role/_default`
- Undeletable, always exists for every account
- Constrained to the `auth.source.coop` IdP only
- Permissions: `{"actions": ["read", "write"], "resources": ["*"]}` — unlimited ceiling, account's actual permissions are the sole constraint
- Account owners can add claim constraints but cannot change the IdP binding

### Claim Constraint Language

Three operators, deliberately minimal:

| Operator | Behavior | Example |
|----------|----------|---------|
| `equals` | Exact string match | `repository` equals `my-org/my-repo` |
| `starts_with` | String prefix match | `ref` starts_with `refs/heads/` |
| `glob` | Wildcard: `*` (any chars), `?` (single char) | `repository` glob `my-org/*` |

Rules:
- All claim values coerced to strings before comparison. Arrays and objects evaluate to false.
- All constraints within a single IdP binding are ANDed.
- Multiple IdP bindings on a Role are ORed.
- Missing claims evaluate to false (fail-closed).
- Top-level claims only — no nested path traversal.
- No regex. Glob is the most expressive operator.

### Permission Statements

Resource pattern format:
```
*                                                        → all resources (unlimited ceiling)
source::{account_id}::product/*                          → all of an account's products
source::{account_id}::product/{product_name}             → entire product
source::{account_id}::product/{product_name}/*           → entire product (equivalent)
source::{account_id}::product/{product_name}/{prefix}/*  → prefix-scoped
source::{account_id}::product/{product_name}/{key}       → single object
```

Rules:
- Resource patterns can reference any account's products. A Role can delegate access to products the account has access to, even if owned by another account or org.
- `*` as the entire resource value means "all resources" — no ceiling; the account's actual permissions are the sole constraint.
- `*` at the end of a pattern matches any suffix (prefix matching). `*` is valid only as the final character or as the entire value.
- Actions are `read` and `write`. `read` maps to `GetObject`, `HeadObject`, `ListObjects`. `write` maps to `PutObject`, `DeleteObject`, and multipart operations.
- Permission statements are additive (allow-only). No explicit denies.
- Roles act as a ceiling — they can never exceed the account's own permissions. The request-time intersection `(Role permissions) ∩ (account's actual permissions)` is the sole enforcement mechanism.

### Role Validation at Creation

1. `name` must match `[a-z0-9][a-z0-9-]{0,62}` (lowercase, hyphens, max 63 chars)
2. Each IdP reference must exist (platform IdP by well-known ID, account IdP by UUID)
3. `max_session_duration` between 900 and 43200 seconds (15 min to 12 hours)
4. At least one identity constraint required
5. At least one permission statement required
6. Maximum 10 IdP bindings per Role, 20 claim constraints per binding, 50 permission statements per Role

## STS Token Exchange

### Endpoint

```
POST /.sts/assume-role-with-web-identity
```

Dot-prefixed account names are reserved as invalid, preventing routing conflicts.

**Request format:** `application/x-www-form-urlencoded`, AWS STS-compatible:
```
Action=AssumeRoleWithWebIdentity
&WebIdentityToken=<JWT>
&RoleArn=source::my-org::role/github-publisher
&RoleSessionName=my-ci-job-42
&DurationSeconds=3600
```

**Response format:** XML, AWS STS-compatible:
```xml
<AssumeRoleWithWebIdentityResponse>
  <AssumeRoleWithWebIdentityResult>
    <Credentials>
      <AccessKeyId>SCSTS...</AccessKeyId>
      <SecretAccessKey>derived-secret</SecretAccessKey>
      <SessionToken>eyJ...</SessionToken>
      <Expiration>2025-03-22T13:00:00Z</Expiration>
    </Credentials>
    <AssumedRoleUser>
      <Arn>source::my-org::role/github-publisher</Arn>
      <AssumedRoleId>SCSTS...:my-ci-job-42</AssumedRoleId>
    </AssumedRoleUser>
  </AssumeRoleWithWebIdentityResult>
</AssumeRoleWithWebIdentityResponse>
```

This format enables `boto3.client('sts', endpoint_url='https://data.source.coop/.sts').assume_role_with_web_identity(...)` with a custom endpoint URL. The `RoleArn` parameter accepts Source Cooperative URN format — the AWS SDK passes the string through without client-side validation.

### Exchange Flow

1. Parse `RoleArn` → extract `account_id` and `role_name`
2. Load Role definition from policy store (cached, 30–60s TTL)
3. Extract `iss` from JWT (without verification)
4. Match `iss` against Role's allowed IdPs — reject immediately if no match
5. Fetch JWKS from the matched IdP (cached, 1hr TTL, 3s timeout, stale-while-revalidate on failure)
6. Verify JWT signature, `exp`, `nbf` (60s clock skew tolerance), and `aud`
7. Evaluate claim constraints for the matched IdP binding
8. Validate `DurationSeconds` ≤ Role's `max_session_duration`
9. Generate credentials and return response

### Credential Issuance

**AccessKeyId:** Random unique identifier prefixed `SCSTS` to distinguish from permanent keys.

**SecretAccessKey:** Derived deterministically: `HMAC-SHA256(server_secret, AccessKeyId)`. Never stored, never transmitted in the SessionToken. The server reconstructs it on each request by re-deriving from the AccessKeyId.

**SessionToken:** A signed JWT (ES256, asymmetric) containing:
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

Key properties:
- **Permissions embedded in the token** avoid per-request policy store lookups for Role ceiling evaluation. The account's underlying permissions are still checked dynamically.
- **`assumed_by`** preserves the original IdP subject for audit trails.
- **`access_key_id`** included so the server can derive the SecretAccessKey via HMAC.
- **SecretAccessKey is NOT in the token.**

### Signing Key Management

- Asymmetric signing: ES256 (ECDSA P-256)
- Private key stored in KMS, used only for token issuance
- Public key served at a JWKS endpoint for verification
- `kid` in JWT header supports key rotation: sign new tokens with new key, accept old key until all tokens signed with it expire
- Rotation procedure: generate new key → sign with new key → old key valid for `max_session_duration` after last use

### Revocation

- Lightweight deny-list of revoked `jti` values stored in Cloudflare KV (or equivalent)
- TTL on each KV entry matches the token's remaining lifetime (self-cleaning)
- Checked on every authenticated request (KV lookup ~1ms at edge)
- `POST /.sts/revoke-session-token` endpoint, callable by account admins

### JWKS Caching

- Cache key: canonicalized issuer URL
- TTL: 1 hour
- Stale-while-revalidate: if JWKS fetch fails and a cached copy exists, serve stale for up to 24 hours (with warning logged)
- If no cache and fetch fails → return `IDPCommunicationError`
- Max response body: 256KB

## Request-Time Authorization

### Step 1: Identify the Caller

- **No credentials** → anonymous
- **Permanent API key** (non-`SCSTS` prefix) → legacy API key lookup via Source API
- **STS credentials** (`SCSTS` prefix) → derive SecretAccessKey via HMAC, verify SigV4, decode SessionToken JWT

### Step 2: Role Action Check (in-memory)

For anonymous callers, only read actions are permitted.

For STS callers, the SessionToken's embedded permissions define the ceiling. If the requested action is not covered, deny immediately.

### Step 3: Resource Resolution

Map the S3 request to a Source Cooperative resource:
- Bucket name → `account_id/product_name`
- Object key → path within the product

### Step 4: Public Resource Early Exit (cached, 60–300s TTL)

For read requests on public products (`data_mode: open`), permit immediately. No further lookups. This is the fast path for the majority of traffic.

### Step 5: Account Permission Lookup (cached, 30–60s TTL)

For non-public resources or write operations:
1. Fetch the account's permissions from the policy store
2. Compute: `(Role ceiling permissions) ∩ (account's actual permissions)`
3. If the intersection includes the requested action on the requested resource → permit
4. Otherwise → deny

### Step 6: Prefix Enforcement

If the Role's permission statement includes a prefix constraint, verify the object key falls within that prefix.

### Authorization Truth Table

| Caller | Resource | Account has access? | Role permits? | Result |
|--------|----------|-------------------|--------------|--------|
| Anonymous | Public product | N/A | N/A | **Allow** (read only) |
| Anonymous | Private product | N/A | N/A | **Deny** |
| STS | Public product, read | N/A | Yes | **Allow** |
| STS | Public product, write | Yes | Yes | **Allow** |
| STS | Private product | Yes | Yes | **Allow** |
| STS | Private product | Yes | No (ceiling) | **Deny** |
| STS | Private product | No | Yes | **Deny** |

## STS Error Responses

Errors use AWS STS XML format for SDK compatibility:

```xml
<ErrorResponse>
  <Error>
    <Code>InvalidIdentityToken</Code>
    <Message>JWT claim 'repository' value 'my-org/wrong-repo' does not match
    constraint 'my-org/correct-repo' on role 'github-publisher'</Message>
  </Error>
</ErrorResponse>
```

| Condition | Error Code | HTTP Status |
|-----------|-----------|-------------|
| Role URN malformed | `MalformedPolicyDocument` | 400 |
| Role not found | `InvalidParameterValue` | 400 |
| JWT malformed or unparseable | `InvalidIdentityToken` | 400 |
| JWT issuer matches no IdP on Role | `InvalidIdentityToken` | 400 |
| JWT signature verification failed | `InvalidIdentityToken` | 400 |
| JWT expired | `ExpiredTokenException` | 400 |
| JWT `aud` mismatch | `InvalidIdentityToken` | 400 |
| Claim constraints not satisfied | `InvalidIdentityToken` | 400 |
| IdP JWKS endpoint unreachable | `IDPCommunicationError` | 400 |
| `DurationSeconds` exceeds max | `ValidationError` | 400 |

Error messages include enough detail for callers to diagnose problems (which claim failed, expected vs. actual values). The Role definition is not secret — the account admin created it.

## Observability

### STS Exchange Logging

Every exchange (success or failure) emits a structured log entry:
```json
{
  "event": "sts_exchange",
  "timestamp": "2025-03-22T12:00:00Z",
  "account_id": "my-org",
  "role_name": "github-publisher",
  "role_urn": "source::my-org::role/github-publisher",
  "idp_issuer": "https://token.actions.githubusercontent.com",
  "assumed_by": "repo:my-org/my-repo:ref:refs/heads/main",
  "session_name": "my-ci-job-42",
  "result": "success",
  "access_key_id": "SCSTS...",
  "duration_seconds": 3600,
  "client_ip": "...",
  "failure_reason": null
}
```

### Request-Time Access Logging

Every S3 request with STS credentials logs:
```json
{
  "event": "s3_request",
  "timestamp": "...",
  "account_id": "my-org",
  "role_name": "github-publisher",
  "session_name": "my-ci-job-42",
  "assumed_by": "repo:my-org/my-repo:ref:refs/heads/main",
  "action": "PutObject",
  "resource": "source::my-org::product/climate-data/2025/data.parquet",
  "result": "allow",
  "client_ip": "..."
}
```

## Migration & Client Tooling

### Coexistence with Current Auth

The STS system runs alongside existing permanent API key auth:
- AccessKeyId prefix `SCSTS` routes to STS credential path; all other prefixes route to legacy API key lookup
- No changes to existing API key auth
- No forced migration timeline — STS is additive

### Anonymous Access

Public data (`data_mode: open`) remains accessible with zero authentication. No STS exchange, no credentials. `--no-sign-request` keeps working. This is a hard requirement.

### Client Tooling (v1)

**GitHub Action: `source-cooperative/configure-credentials`**
```yaml
permissions:
  id-token: write
steps:
  - uses: source-cooperative/configure-credentials@v1
    with:
      role-urn: source::my-org::role/github-publisher
  - run: aws s3 cp data.parquet s3://data.source.coop/my-org/my-product/
```

Requests a GitHub OIDC token with audience `https://data.source.coop`, calls the STS endpoint, and exports `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`. All downstream tools pick these up automatically.

**source-coop CLI (`source-cooperative/source-coop-cli`)**

The existing CLI already supports `source login` and `credential_process` integration. The STS work extends it so that login calls the new STS endpoint.

Users configure `~/.aws/config` with role-specific profiles:
```ini
[profile source-read]
credential_process = source-coop creds --role-arn source::my-org::role/reader

[profile source-write]
credential_process = source-coop creds --role-arn source::my-org::role/publisher
```

The `source-coop creds` command: checks for cached valid credentials → if expired, triggers OIDC login (or uses cached auth.source.coop token) → calls STS endpoint → returns credentials in `credential_process` JSON format.

Users select the profile per tool:
```bash
aws s3 ls s3://data.source.coop/ --profile source-read
```

**Direct SDK usage** for programmatic integrations:
```python
sts = boto3.client('sts', endpoint_url='https://data.source.coop/.sts')
creds = sts.assume_role_with_web_identity(
    RoleArn='source::my-org::role/github-publisher',
    WebIdentityToken=token,
    RoleSessionName='my-job'
)
```

## Role Management API

```
POST   /api/accounts/{account_id}/idps
GET    /api/accounts/{account_id}/idps
DELETE /api/accounts/{account_id}/idps/{idp_id}

POST   /api/accounts/{account_id}/roles
GET    /api/accounts/{account_id}/roles
GET    /api/accounts/{account_id}/roles/{role_name}
PUT    /api/accounts/{account_id}/roles/{role_name}
DELETE /api/accounts/{account_id}/roles/{role_name}
```

Only account owners and org admins can manage IdPs and Roles. The `_default` Role cannot be deleted.
