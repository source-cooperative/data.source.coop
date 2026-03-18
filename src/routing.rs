//! URL parsing and rewriting for Source Cooperative paths.
//!
//! Translates `/{account}/{product}/{key}` paths into multistore's virtual
//! bucket model where bucket = `{account}--{product}`.

/// Info for rewriting list responses when the request was routed via prefix.
pub struct PrefixRouteInfo {
    /// The account name (used for `<Name>` rewriting).
    pub account: String,
    /// The product name (used for prepending to keys).
    pub product: String,
    /// The original prefix as seen by the client (used for `<Prefix>` rewriting).
    pub original_prefix: String,
}

/// The result of parsing an incoming request URL.
pub enum ParsedRequest {
    /// Root index: `GET /`
    Index,
    /// Object operation: `GET/HEAD /{account}/{product}/{key}`
    ObjectRequest {
        rewritten_path: String,
        query: Option<String>,
    },
    /// List with product prefix: `GET /{account}?list-type=2&prefix=product/...`
    ProductList {
        rewritten_path: String,
        query: String,
        /// When the request was routed via prefix (e.g. `?prefix=product/...`),
        /// this contains the product name so we can rewrite keys in the response.
        /// `None` for segment-routed requests (`/{account}/{product}?list-type=2`).
        prefix_route: Option<PrefixRouteInfo>,
    },
    /// List products for an account: `GET /{account}?list-type=2` (no product in prefix)
    AccountList {
        account: String,
        #[allow(dead_code)]
        query: String,
    },
    /// Write operation — reject with 405
    WriteNotAllowed,
    /// Bad request
    BadRequest(String),
}

/// Parse an incoming Source Cooperative request and determine how to handle it.
pub fn parse_request(method: &http::Method, path: &str, query: Option<&str>) -> ParsedRequest {
    // Reject write methods
    if matches!(
        *method,
        http::Method::PUT | http::Method::POST | http::Method::DELETE | http::Method::PATCH
    ) {
        return ParsedRequest::WriteNotAllowed;
    }

    let trimmed = path.trim_start_matches('/');

    // Root
    if trimmed.is_empty() {
        return ParsedRequest::Index;
    }

    let segments: Vec<&str> = trimmed.splitn(3, '/').collect();

    match segments.len() {
        // /{account} — list operation or bad request
        1 => {
            let account = segments[0];
            let query_str = query.unwrap_or("");

            if is_list_request(query_str) {
                if let Some(prefix) = extract_query_param(query_str, "prefix") {
                    if !prefix.is_empty() {
                        return route_list_with_prefix(account, prefix, query_str);
                    }
                }
                // No prefix — list products for this account
                return ParsedRequest::AccountList {
                    account: account.to_string(),
                    query: query_str.to_string(),
                };
            }
            ParsedRequest::BadRequest("Missing product in path".to_string())
        }
        // /{account}/{product} or /{account}/{product}/{key...}
        _ => {
            let account = segments[0];
            let product = segments[1];
            let key = if segments.len() == 3 { segments[2] } else { "" };
            let bucket = format!("{}--{}", account, product);

            // /{account}/{product}?list-type=2 → product-level list
            if key.is_empty() && query.is_some_and(is_list_request) {
                return ParsedRequest::ProductList {
                    rewritten_path: format!("/{}", bucket),
                    query: query.unwrap_or("").to_string(),
                    prefix_route: None,
                };
            }

            let rewritten_path = if key.is_empty() {
                format!("/{}", bucket)
            } else {
                format!("/{}/{}", bucket, key)
            };

            ParsedRequest::ObjectRequest {
                rewritten_path,
                query: query.map(|s| s.to_string()),
            }
        }
    }
}

/// Route a list request where the prefix contains a product name.
fn route_list_with_prefix(account: &str, prefix: &str, query_str: &str) -> ParsedRequest {
    if let Some(slash_pos) = prefix.find('/') {
        let product = &prefix[..slash_pos];
        let remaining_prefix = &prefix[slash_pos + 1..];
        let bucket = format!("{}--{}", account, product);
        let new_query = rewrite_prefix_in_query(query_str, remaining_prefix);
        ParsedRequest::ProductList {
            rewritten_path: format!("/{}", bucket),
            query: new_query,
            prefix_route: Some(PrefixRouteInfo {
                account: account.to_string(),
                product: product.to_string(),
                original_prefix: prefix.to_string(),
            }),
        }
    } else {
        // prefix is just "product" without trailing slash — list all objects in product
        let bucket = format!("{}--{}", account, prefix);
        let new_query = rewrite_prefix_in_query(query_str, "");
        ParsedRequest::ProductList {
            rewritten_path: format!("/{}", bucket),
            query: new_query,
            prefix_route: Some(PrefixRouteInfo {
                account: account.to_string(),
                product: prefix.to_string(),
                original_prefix: prefix.to_string(),
            }),
        }
    }
}

fn is_list_request(query: &str) -> bool {
    query.contains("list-type=")
}

fn extract_query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        pair.split_once('=')
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v)
    })
}

fn rewrite_prefix_in_query(query: &str, new_prefix: &str) -> String {
    query
        .split('&')
        .map(|pair| {
            if pair.starts_with("prefix=") {
                format!("prefix={}", new_prefix)
            } else {
                pair.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}
