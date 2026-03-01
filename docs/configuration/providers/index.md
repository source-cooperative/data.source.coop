# Config Providers

The proxy loads its configuration (buckets, roles, credentials) through the `ConfigProvider` trait. Multiple backends are available, selectable at build time via feature flags.

## ConfigProvider Trait

```rust
pub trait ConfigProvider: Clone + MaybeSend + MaybeSync + 'static {
    async fn list_buckets(&self) -> Result<Vec<BucketConfig>, ProxyError>;
    async fn get_bucket(&self, name: &str) -> Result<Option<BucketConfig>, ProxyError>;
    async fn get_role(&self, role_id: &str) -> Result<Option<RoleConfig>, ProxyError>;
    async fn get_credential(&self, access_key_id: &str)
        -> Result<Option<StoredCredential>, ProxyError>;
}
```

## Available Providers

| Provider | Feature Flag | Best For |
|----------|-------------|----------|
| [Static File](./static-file) | (always available) | Simple deployments, single-file config |
| [HTTP API](./http) | `config-http` | Centralized config service, control planes |
| [DynamoDB](./dynamodb) | `config-dynamodb` | AWS-native infrastructure |
| [PostgreSQL](./postgres) | `config-postgres` | Database-backed config |

All providers can be wrapped with [CachedProvider](./cached) for in-memory caching with TTL-based expiration.

## Implementing a Custom Provider

Implement the `ConfigProvider` trait and wrap it in `DefaultResolver` to get standard S3 proxy behavior:

```rust
use source_coop_core::config::ConfigProvider;
use source_coop_core::error::ProxyError;
use source_coop_core::types::*;

#[derive(Clone)]
struct MyProvider { /* ... */ }

impl ConfigProvider for MyProvider {
    async fn list_buckets(&self) -> Result<Vec<BucketConfig>, ProxyError> {
        todo!()
    }
    async fn get_bucket(&self, name: &str) -> Result<Option<BucketConfig>, ProxyError> {
        todo!()
    }
    async fn get_role(&self, role_id: &str) -> Result<Option<RoleConfig>, ProxyError> {
        todo!()
    }
    async fn get_credential(&self, access_key_id: &str)
        -> Result<Option<StoredCredential>, ProxyError> {
        todo!()
    }
}
```

See [Custom Config Provider](/extending/custom-provider) for a full guide.
