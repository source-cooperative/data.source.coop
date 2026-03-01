# Custom Config Provider

The `ConfigProvider` trait defines how the proxy loads buckets, roles, and credentials. Implement it to plug in your own configuration backend.

## The Trait

```rust
use source_coop_core::config::ConfigProvider;
use source_coop_core::error::ProxyError;
use source_coop_core::types::*;

pub trait ConfigProvider: Clone + MaybeSend + MaybeSync + 'static {
    async fn list_buckets(&self) -> Result<Vec<BucketConfig>, ProxyError>;
    async fn get_bucket(&self, name: &str) -> Result<Option<BucketConfig>, ProxyError>;
    async fn get_role(&self, role_id: &str) -> Result<Option<RoleConfig>, ProxyError>;
    async fn get_credential(&self, access_key_id: &str)
        -> Result<Option<StoredCredential>, ProxyError>;
}
```

## Example: Redis Provider

```rust
use source_coop_core::config::ConfigProvider;
use source_coop_core::error::ProxyError;
use source_coop_core::types::*;

#[derive(Clone)]
struct RedisProvider {
    client: redis::Client,
}

impl ConfigProvider for RedisProvider {
    async fn list_buckets(&self) -> Result<Vec<BucketConfig>, ProxyError> {
        let mut conn = self.client.get_async_connection().await
            .map_err(|e| ProxyError::Internal(e.to_string()))?;

        let keys: Vec<String> = redis::cmd("KEYS")
            .arg("bucket:*")
            .query_async(&mut conn)
            .await
            .map_err(|e| ProxyError::Internal(e.to_string()))?;

        let mut buckets = Vec::new();
        for key in keys {
            let json: String = redis::cmd("GET")
                .arg(&key)
                .query_async(&mut conn)
                .await
                .map_err(|e| ProxyError::Internal(e.to_string()))?;
            let bucket: BucketConfig = serde_json::from_str(&json)
                .map_err(|e| ProxyError::ConfigError(e.to_string()))?;
            buckets.push(bucket);
        }
        Ok(buckets)
    }

    async fn get_bucket(&self, name: &str) -> Result<Option<BucketConfig>, ProxyError> {
        // Similar Redis GET with key "bucket:{name}"
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

## Using with DefaultResolver

Wrap your provider in `DefaultResolver` to get standard S3 proxy behavior (path/virtual-host parsing, SigV4 auth, scope-based authorization):

```rust
use source_coop_core::resolver::DefaultResolver;
use source_coop_core::config::cached::CachedProvider;
use std::time::Duration;

// Optional: wrap with caching
let cached = CachedProvider::new(redis_provider, Duration::from_secs(60));

// Create resolver with optional token key and domain
let resolver = DefaultResolver::new(cached, token_key, virtual_host_domain);

// Wire into the proxy handler
let handler = ProxyHandler::new(backend, resolver);
```

## Using with CachedProvider

For network-backed providers, wrap with `CachedProvider` to reduce latency:

```rust
use source_coop_core::config::cached::CachedProvider;
use std::time::Duration;

let provider = CachedProvider::new(redis_provider, Duration::from_secs(120));
```

See [Caching](/configuration/providers/cached) for cache behavior details.
