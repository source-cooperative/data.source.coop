# Credentials

Long-lived credentials are static access key pairs stored in the proxy configuration. They work like standard AWS IAM access keys — clients sign requests using SigV4 with the access key ID and secret access key.

## Configuration

```toml
[[credentials]]
access_key_id = "AKPROXY00000EXAMPLE"
secret_access_key = "proxy/secret/key/EXAMPLE000000000000"
principal_name = "internal-dashboard"
created_at = "2024-01-15T00:00:00Z"
enabled = true

[[credentials.allowed_scopes]]
bucket = "public-data"
prefixes = []
actions = ["get_object", "head_object", "list_bucket"]

[[credentials.allowed_scopes]]
bucket = "ml-artifacts"
prefixes = ["models/production/"]
actions = ["get_object", "head_object"]
```

## Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `access_key_id` | string | Yes | Access key identifier |
| `secret_access_key` | string | Yes | Secret key for SigV4 signing |
| `principal_name` | string | Yes | Human-readable name for the credential holder |
| `created_at` | datetime | Yes | When the credential was created (ISO 8601) |
| `expires_at` | datetime | No | When the credential expires (omit for no expiration) |
| `enabled` | bool | Yes | Whether the credential is active |
| `allowed_scopes` | AccessScope[] | Yes | Buckets, prefixes, and actions granted |

## Access Scopes

Scopes work identically to [role scopes](./roles#access-scopes) — each scope specifies a bucket, optional prefix restrictions, and allowed actions.

## When to Use Long-Lived Credentials

Long-lived credentials are appropriate for:

- **Service accounts** that need persistent access without OIDC
- **Internal tools** where token exchange adds unnecessary complexity
- **Development and testing** environments
- **Environments without an OIDC provider**

> [!TIP]
> For CI/CD workflows and user-facing applications, prefer [OIDC/STS temporary credentials](/auth/proxy-auth#oidcsts-temporary-credentials) — they expire automatically and avoid storing secrets in config.

## Disabling Credentials

Set `enabled = false` to immediately revoke access without removing the credential from config:

```toml
[[credentials]]
access_key_id = "AKPROXY00000REVOKED"
secret_access_key = "..."
principal_name = "old-service"
created_at = "2023-01-01T00:00:00Z"
enabled = false
```

Disabled credentials return `AccessDenied` for any request.
