//! Route handlers for Source Coop-specific endpoints.
//!
//! These handlers are registered with the gateway's Router so that
//! custom endpoints are checked before the S3 proxy pipeline runs.

use multistore::api::list::parse_list_query_params;
use multistore::api::response::{ListBucketResult, ListCommonPrefix};
use multistore::route_handler::{ProxyResult, RequestInfo, RouteHandler, RouteHandlerFuture};

use crate::pagination::paginate_prefixes;
use crate::registry::SourceCoopRegistry;

const VERSION: &str = env!("CARGO_PKG_VERSION");

// ── IndexHandler ────────────────────────────────────────────────────

/// Returns a version string for `GET /`.
pub struct IndexHandler;

impl RouteHandler for IndexHandler {
    fn handle<'a>(&'a self, req: &'a RequestInfo<'a>) -> RouteHandlerFuture<'a> {
        Box::pin(async move {
            if req.method == http::Method::GET {
                Some(ProxyResult::json(
                    200,
                    format!("Source Cooperative Data Proxy v{}", VERSION),
                ))
            } else {
                None
            }
        })
    }
}

// ── AccountListHandler ──────────────────────────────────────────────

/// Lists products for an account at `GET /{bucket}?list-type=2`.
///
/// Falls through to the gateway when the bucket param contains the
/// separator (meaning it's already a rewritten `account:product` path),
/// or when the request isn't a list operation.
pub struct AccountListHandler {
    registry: SourceCoopRegistry,
    bucket_separator: String,
}

impl AccountListHandler {
    pub fn new(registry: SourceCoopRegistry, mapping: &multistore_path_mapping::PathMapping) -> Self {
        Self {
            registry,
            bucket_separator: mapping.bucket_separator.clone(),
        }
    }
}

impl RouteHandler for AccountListHandler {
    fn handle<'a>(&'a self, req: &'a RequestInfo<'a>) -> RouteHandlerFuture<'a> {
        Box::pin(async move {
            let bucket = match req.params.get("bucket") {
                Some(b) => b,
                None => return None,
            };

            // Already rewritten (e.g. "cholmes:admin-boundaries") → fall through
            if bucket.contains(&self.bucket_separator) {
                return None;
            }

            // Only handle list requests (must have list-type= query param)
            let query_str = req.query.unwrap_or("");
            if !query_str.split('&').any(|p| p.starts_with("list-type=")) {
                return None;
            }

            // List products for this account
            let account = bucket;
            match self.registry.list_products(account).await {
                Ok(products) => {
                    let params = parse_list_query_params(req.query);
                    let all_prefixes: Vec<String> =
                        products.into_iter().map(|p| format!("{p}/")).collect();
                    let paginated = paginate_prefixes(all_prefixes, &params);

                    let common_prefixes: Vec<ListCommonPrefix> = paginated
                        .prefixes
                        .into_iter()
                        .map(|prefix| ListCommonPrefix { prefix })
                        .collect();

                    let result = ListBucketResult {
                        xmlns: "http://s3.amazonaws.com/doc/2006-03-01/",
                        name: account.to_string(),
                        prefix: String::new(),
                        delimiter: "/".to_string(),
                        encoding_type: params.encoding_type,
                        max_keys: params.max_keys,
                        is_truncated: paginated.is_truncated,
                        key_count: common_prefixes.len(),
                        start_after: params.start_after,
                        continuation_token: params.continuation_token,
                        next_continuation_token: paginated.next_continuation_token,
                        contents: vec![],
                        common_prefixes,
                    };

                    Some(ProxyResult::xml(200, result.to_xml()))
                }
                Err(e) => {
                    tracing::error!("AccountList({}) error: {:?}", account, e);
                    let err = multistore::api::response::ErrorResponse {
                        code: "BadGateway".to_string(),
                        message: "Failed to list products from upstream API".to_string(),
                        resource: String::new(),
                        request_id: self.registry.request_id.clone(),
                    };
                    Some(ProxyResult::xml(502, err.to_xml()))
                }
            }
        })
    }
}
