# Custom Request Resolver

The `RequestResolver` trait controls how incoming requests are parsed, authenticated, and authorized. Implement it for full control over the request handling pipeline.

## The Trait

```rust
use source_coop_core::resolver::{RequestResolver, ResolvedAction};
use source_coop_core::error::ProxyError;
use http::{Method, HeaderMap};

pub trait RequestResolver: Clone + MaybeSend + MaybeSync + 'static {
    fn resolve(
        &self,
        method: &Method,
        path: &str,
        query: Option<&str>,
        headers: &HeaderMap,
    ) -> impl Future<Output = Result<ResolvedAction, ProxyError>> + MaybeSend;
}
```

## ResolvedAction

The resolver returns one of two actions:

```rust
pub enum ResolvedAction {
    /// Forward to a backend (standard proxy behavior)
    Proxy {
        operation: S3Operation,
        bucket_config: BucketConfig,
        list_rewrite: Option<ListRewrite>,
    },
    /// Return a synthetic response (e.g., virtual listing, redirect)
    Response {
        status: StatusCode,
        headers: HeaderMap,
        body: ProxyResponseBody,
    },
}
```

## Example: Custom Namespace

A resolver that maps `/{account}/{repo}/{key}` to backend buckets:

```rust
use source_coop_core::resolver::{RequestResolver, ResolvedAction};
use source_coop_core::s3::request::build_s3_operation;
use source_coop_core::error::ProxyError;

#[derive(Clone)]
struct MyResolver {
    api_client: ApiClient,
}

impl RequestResolver for MyResolver {
    async fn resolve(
        &self,
        method: &Method,
        path: &str,
        query: Option<&str>,
        headers: &HeaderMap,
    ) -> Result<ResolvedAction, ProxyError> {
        // Parse custom URL structure
        let parts: Vec<&str> = path.trim_start_matches('/').splitn(3, '/').collect();
        let (account, repo, key) = match parts.as_slice() {
            [a, r, k] => (*a, *r, *k),
            [a, r] => (*a, *r, ""),
            _ => return Err(ProxyError::BucketNotFound),
        };

        // Look up the backend config from an external API
        let bucket_config = self.api_client
            .get_backend(account, repo)
            .await
            .map_err(|_| ProxyError::BucketNotFound)?;

        // Authenticate via external service
        self.api_client
            .check_permissions(account, repo, headers)
            .await
            .map_err(|_| ProxyError::AccessDenied)?;

        // Build the S3 operation from method + key
        let operation = build_s3_operation(method, &bucket_config.name, key, query)?;

        Ok(ResolvedAction::Proxy {
            operation,
            bucket_config,
            list_rewrite: None,
        })
    }
}
```

## Wiring Into the Handler

```rust
let resolver = MyResolver::new(api_client);
let handler = ProxyHandler::new(backend, resolver);

// In your request handler:
let action = handler.resolve_request(method, path, query, &headers).await;
match action {
    HandlerAction::Forward(fwd) => { /* execute presigned URL */ }
    HandlerAction::Response(res) => { /* return response */ }
    HandlerAction::NeedsBody(pending) => { /* collect body, call handle_with_body */ }
}
```

## ListRewrite

The `ListRewrite` option in `ResolvedAction::Proxy` allows you to transform `<Key>` and `<Prefix>` values in LIST response XML:

```rust
ResolvedAction::Proxy {
    operation,
    bucket_config,
    list_rewrite: Some(ListRewrite {
        strip_prefix: "internal/mirror/".to_string(),
        add_prefix: "public/".to_string(),
    }),
}
```

This is useful when the backend key structure differs from what clients expect.
