# Static File Provider

The static file provider loads configuration from a TOML or JSON file at startup. No feature flags required — it's always available.

## Usage

```rust
use source_coop_core::config::static_file::StaticProvider;

// From a TOML file
let provider = StaticProvider::from_file("config.toml")?;

// From a TOML string
let provider = StaticProvider::from_toml(include_str!("../config.toml"))?;

// From a JSON string (useful for CF Workers env vars)
let provider = StaticProvider::from_json(&json_string)?;
```

## Config Format

### TOML

```toml
[[buckets]]
name = "my-data"
backend_type = "s3"
anonymous_access = true

[buckets.backend_options]
endpoint = "https://s3.us-east-1.amazonaws.com"
bucket_name = "my-backend-bucket"
region = "us-east-1"

[[roles]]
role_id = "my-role"
name = "My Role"
trusted_oidc_issuers = ["https://auth.example.com"]
subject_conditions = ["*"]
max_session_duration_secs = 3600

[[roles.allowed_scopes]]
bucket = "my-data"
prefixes = []
actions = ["get_object", "head_object"]

[[credentials]]
access_key_id = "AKEXAMPLE"
secret_access_key = "secret"
principal_name = "service"
created_at = "2024-01-01T00:00:00Z"
enabled = true

[[credentials.allowed_scopes]]
bucket = "my-data"
prefixes = []
actions = ["get_object"]
```

### JSON

```json
{
  "buckets": [{
    "name": "my-data",
    "backend_type": "s3",
    "anonymous_access": true,
    "backend_options": {
      "endpoint": "https://s3.us-east-1.amazonaws.com",
      "bucket_name": "my-backend-bucket",
      "region": "us-east-1"
    }
  }],
  "roles": [],
  "credentials": []
}
```

## When to Use

- Simple deployments with a single config file
- Baked-in configuration (e.g., compiled into the binary with `include_str!`)
- Cloudflare Workers (JSON via `PROXY_CONFIG` env var)
- Development and testing

For dynamic configuration that changes without redeployment, consider [HTTP](./http), [DynamoDB](./dynamodb), or [PostgreSQL](./postgres) providers.
