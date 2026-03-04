# Configuration Example

A complete, annotated configuration file showing all available options.

```toml
# =============================================================================
# Virtual Buckets
# =============================================================================

# A publicly accessible S3 bucket (anonymous reads allowed)
[[buckets]]
name = "public-data"                    # Client-visible bucket name
backend_type = "s3"                     # Backend provider: "s3", "az", or "gcs"
anonymous_access = true                 # Allow GET/HEAD/LIST without auth
allowed_roles = []                      # No STS roles (anonymous only)

[buckets.backend_options]
endpoint = "https://s3.us-east-1.amazonaws.com"
bucket_name = "my-company-public-assets"  # Actual backend bucket name
region = "us-east-1"
access_key_id = "AKIAIOSFODNN7EXAMPLE"
secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"

# A private S3 bucket backed by MinIO with a backend prefix
[[buckets]]
name = "ml-artifacts"
backend_type = "s3"
backend_prefix = "v2"                   # Prepend "v2/" to all keys when forwarding
anonymous_access = false
allowed_roles = ["github-actions-deployer"]

[buckets.backend_options]
endpoint = "https://minio.internal:9000"
bucket_name = "ml-pipeline-artifacts"
region = "us-east-1"
access_key_id = "minioadmin"
secret_access_key = "minioadmin"

# An S3 bucket on a different region
[[buckets]]
name = "deploy-bundles"
backend_type = "s3"
anonymous_access = false
allowed_roles = ["github-actions-deployer", "ci-readonly"]

[buckets.backend_options]
endpoint = "https://s3.us-west-2.amazonaws.com"
bucket_name = "prod-deploy-bundles"
region = "us-west-2"
access_key_id = "AKIAI44QH8DHBEXAMPLE"
secret_access_key = "je7MtGbClwBF/2Zp9Utk/h3yCo8nvbEXAMPLEKEY"

# An Azure Blob Storage backend (requires "azure" feature)
[[buckets]]
name = "azure-data"
backend_type = "az"
anonymous_access = true
allowed_roles = []

[buckets.backend_options]
account_name = "mystorageaccount"
container_name = "public-datasets"

# =============================================================================
# IAM Roles (for STS AssumeRoleWithWebIdentity)
# =============================================================================

# Role for GitHub Actions CI/CD pipelines
[[roles]]
role_id = "github-actions-deployer"         # Used as RoleArn in STS requests
name = "GitHub Actions Deploy Role"
trusted_oidc_issuers = ["https://token.actions.githubusercontent.com"]
required_audience = "sts.s3proxy.example.com"  # Token's `aud` must match

# Glob patterns for the `sub` claim
subject_conditions = [
    "repo:myorg/myapp:ref:refs/heads/main",
    "repo:myorg/myapp:ref:refs/heads/release/*",
    "repo:myorg/infrastructure:*",
]
max_session_duration_secs = 3600            # 1 hour max

# Scopes granted to minted credentials
[[roles.allowed_scopes]]
bucket = "ml-artifacts"
prefixes = ["models/", "datasets/"]         # Restrict to these prefixes
actions = [
    "get_object", "head_object", "put_object",
    "create_multipart_upload", "upload_part", "complete_multipart_upload"
]

[[roles.allowed_scopes]]
bucket = "deploy-bundles"
prefixes = []                               # Full bucket access
actions = [
    "get_object", "head_object", "put_object",
    "create_multipart_upload", "upload_part", "complete_multipart_upload"
]

# Role with template variables for per-user access
[[roles]]
role_id = "source-user"
name = "Source Cooperative User"
trusted_oidc_issuers = ["https://auth.source.coop", "https://auth.staging.source.coop"]
subject_conditions = ["*"]                  # Any subject
max_session_duration_secs = 3600

# {sub} is replaced with the JWT's `sub` claim at mint time
[[roles.allowed_scopes]]
bucket = "{sub}"
prefixes = []
actions = ["get_object", "head_object", "put_object", "list_bucket"]

# Read-only role for CI
[[roles]]
role_id = "ci-readonly"
name = "CI Read-Only Role"
trusted_oidc_issuers = ["https://token.actions.githubusercontent.com"]
subject_conditions = ["repo:myorg/*"]       # Any repo in the org
max_session_duration_secs = 1800            # 30 minutes

[[roles.allowed_scopes]]
bucket = "deploy-bundles"
prefixes = []
actions = ["get_object", "head_object", "list_bucket"]

# =============================================================================
# Long-Lived Credentials
# =============================================================================

# Service account for an internal tool
[[credentials]]
access_key_id = "AKPROXY00000EXAMPLE"
secret_access_key = "proxy/secret/key/EXAMPLE000000000000"
principal_name = "internal-dashboard"
created_at = "2024-01-15T00:00:00Z"
enabled = true                              # Set to false to revoke

[[credentials.allowed_scopes]]
bucket = "public-data"
prefixes = []
actions = ["get_object", "head_object", "list_bucket"]

[[credentials.allowed_scopes]]
bucket = "ml-artifacts"
prefixes = ["models/production/"]
actions = ["get_object", "head_object"]
```

## Environment Variables

These are set separately from the config file:

```bash
# Required for STS temporary credentials (sealed tokens)
export SESSION_TOKEN_KEY=$(openssl rand -base64 32)

# Required for OIDC backend auth
export OIDC_PROVIDER_KEY=$(cat oidc-key.pem)
export OIDC_PROVIDER_ISSUER="https://data.source.coop"

# Logging
export RUST_LOG="source_coop=info"
```
