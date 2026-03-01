# Buckets

Buckets define the virtual namespaces that clients interact with. Each bucket maps a client-visible name to a backend object store.

## Configuration

```toml
[[buckets]]
name = "my-data"
backend_type = "s3"
backend_prefix = "v2"
anonymous_access = false
allowed_roles = ["github-actions-deployer"]

[buckets.backend_options]
endpoint = "https://s3.us-east-1.amazonaws.com"
bucket_name = "my-backend-bucket"
region = "us-east-1"
access_key_id = "AKIAIOSFODNN7EXAMPLE"
secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
```

## Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Client-visible bucket name |
| `backend_type` | string | Yes | Backend provider: `"s3"`, `"az"`, or `"gcs"` |
| `backend_prefix` | string | No | Prefix prepended to keys when forwarding to the backend |
| `anonymous_access` | bool | No | Allow GET/HEAD/LIST without authentication (default: `false`) |
| `allowed_roles` | string[] | No | Role IDs that can be assumed for this bucket |
| `backend_options` | map | Yes | Provider-specific configuration (see below) |

## Backend Options by Provider

### S3 / MinIO / R2

```toml
[buckets.backend_options]
endpoint = "https://s3.us-east-1.amazonaws.com"
bucket_name = "my-backend-bucket"
region = "us-east-1"
access_key_id = "AKIA..."
secret_access_key = "..."
```

| Option | Required | Description |
|--------|----------|-------------|
| `endpoint` | Yes | S3 endpoint URL |
| `bucket_name` | Yes | Backend bucket name |
| `region` | Yes | AWS region |
| `access_key_id` | No | AWS access key (omit for anonymous or OIDC) |
| `secret_access_key` | No | AWS secret key |
| `skip_signature` | No | Set to `"true"` for unsigned requests |

### Azure Blob Storage

::: info
Requires the `azure` feature flag on `source-coop-core`. Enabled by default in the server runtime, not available in CF Workers.
:::

```toml
[buckets.backend_options]
account_name = "mystorageaccount"
container_name = "my-container"
access_key = "..."
```

| Option | Required | Description |
|--------|----------|-------------|
| `account_name` | Yes | Azure storage account name |
| `container_name` | Yes | Blob container name |
| `access_key` | No | Storage account access key |
| `skip_signature` | No | Set to `"true"` for anonymous access |

### Google Cloud Storage

::: info
Requires the `gcp` feature flag on `source-coop-core`. Enabled by default in the server runtime, not available in CF Workers.
:::

```toml
[buckets.backend_options]
bucket_name = "my-gcs-bucket"
service_account_key = '{"type": "service_account", ...}'
```

| Option | Required | Description |
|--------|----------|-------------|
| `bucket_name` | Yes | GCS bucket name |
| `service_account_key` | No | JSON service account key |
| `skip_signature` | No | Set to `"true"` for anonymous access |

### OIDC Backend Auth Options

For any backend type, you can use OIDC-based credential resolution instead of static credentials:

```toml
[buckets.backend_options]
endpoint = "https://s3.us-east-1.amazonaws.com"
bucket_name = "my-backend-bucket"
region = "us-east-1"
auth_type = "oidc"
oidc_role_arn = "arn:aws:iam::123456789012:role/ProxyRole"
oidc_subject = "my-connection"  # optional, defaults to "s3-proxy"
```

See [Authenticating with Backends](/auth/backend-auth) for setup details.

## Backend Prefix

The `backend_prefix` field transparently prepends a path prefix to all keys when forwarding requests to the backend. Clients don't see this prefix.

```toml
[[buckets]]
name = "ml-artifacts"
backend_prefix = "v2"

[buckets.backend_options]
bucket_name = "ml-pipeline-artifacts"
```

With this configuration:
- Client requests `GET /ml-artifacts/models/latest.pt`
- Proxy forwards to backend key `v2/models/latest.pt` in `ml-pipeline-artifacts`
- LIST responses have the prefix stripped so clients see `models/latest.pt`

## Anonymous Access

Setting `anonymous_access = true` allows unauthenticated GET, HEAD, and LIST requests. Write operations (PUT, DELETE, multipart) always require authentication regardless of this setting.

```toml
[[buckets]]
name = "public-data"
anonymous_access = true
```
