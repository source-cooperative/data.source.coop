# HTTP API Provider

The HTTP provider fetches configuration from a centralized REST API. Useful when you have a control plane service that manages proxy configuration.

## Feature Flag

```bash
cargo build -p source-coop-server --features source-coop-core/config-http
```

## Usage

```rust
use source_coop_core::config::http::HttpProvider;

let provider = HttpProvider::new(
    "https://config-api.internal:8080".to_string(),
    Some("Bearer my-api-token".to_string()),
);
```

## Expected API Endpoints

The HTTP provider expects a REST API with these endpoints:

| Endpoint | Method | Returns |
|----------|--------|---------|
| `/buckets` | GET | `Vec<BucketConfig>` |
| `/buckets/{name}` | GET | `Option<BucketConfig>` |
| `/roles/{id}` | GET | `Option<RoleConfig>` |
| `/credentials/{access_key_id}` | GET | `Option<StoredCredential>` |

All responses should be JSON-encoded. Missing resources should return `null` or a 404 status.

## When to Use

- Centralized config management across multiple proxy instances
- Dynamic configuration that changes without proxy restarts (when combined with [caching](./cached))
- Integration with a custom control plane or admin dashboard
