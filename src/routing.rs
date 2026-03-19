use multistore_path_mapping::PathMapping;

#[derive(Debug, PartialEq)]
pub enum RequestClass {
    /// Root index: `GET /`
    Index,
    /// Bad request
    BadRequest(String),
    /// List products for an account: `GET /{account}?list-type=2` (no product prefix)
    AccountList { account: String, query: Option<String> },
    /// Everything else goes through the gateway with a rewritten path
    ProxyRequest {
        rewritten_path: String,
        query: Option<String>,
    },
}

/// Classify an incoming request into one of the handled cases.
pub fn classify_request(
    mapping: &PathMapping,
    path: &str,
    query: Option<&str>,
) -> RequestClass {
    let trimmed = path.trim_matches('/');

    // Root
    if trimmed.is_empty() {
        return RequestClass::Index;
    }

    // Try mapping the path (works for /{account}/{product}[/{key}])
    if let Some(mapped) = mapping.parse(path) {
        let rewritten_path = match mapped.key {
            Some(ref key) => format!("/{}/{}", mapped.bucket, key),
            None => format!("/{}", mapped.bucket),
        };
        return RequestClass::ProxyRequest {
            rewritten_path,
            query: query.map(|q| q.to_string()),
        };
    }

    // Single segment: /{account} — must be a list or prefix-routed request
    let segments: Vec<&str> = trimmed.splitn(2, '/').collect();
    if segments.len() == 1 {
        let account = segments[0];
        let query_str = query.unwrap_or("");

        if is_list_request(query_str) {
            if let Some(prefix) = extract_query_param(query_str, "prefix") {
                if !prefix.is_empty() {
                    return route_list_with_prefix(mapping, account, &prefix, query_str);
                }
            }
            // No prefix — list products for this account
            return RequestClass::AccountList {
                account: account.to_string(),
                query: if query_str.is_empty() { None } else { Some(query_str.to_string()) },
            };
        }
        return RequestClass::BadRequest("Missing product in path".to_string());
    }

    // Shouldn't reach here, but fall back to bad request
    RequestClass::BadRequest("Invalid request".to_string())
}

/// Route a list request where the prefix contains a product name.
///
/// `GET /{account}?list-type=2&prefix=product/subdir/` becomes
/// a proxy request to `/{account--product}?list-type=2&prefix=subdir/`.
fn route_list_with_prefix(
    mapping: &PathMapping,
    account: &str,
    prefix: &str,
    query_str: &str,
) -> RequestClass {
    let (product, remaining_prefix) = if let Some(slash_pos) = prefix.find('/') {
        (&prefix[..slash_pos], &prefix[slash_pos + 1..])
    } else {
        (prefix, "")
    };

    let bucket = format!("{}{}{}", account, mapping.bucket_separator, product);
    let new_query = rewrite_prefix_in_query(query_str, remaining_prefix);

    RequestClass::ProxyRequest {
        rewritten_path: format!("/{}", bucket),
        query: Some(new_query),
    }
}

pub fn is_list_request(query: &str) -> bool {
    query.contains("list-type=")
}

pub fn extract_query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        pair.split_once('=')
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| {
                percent_encoding::percent_decode_str(v)
                    .decode_utf8_lossy()
                    .into_owned()
            })
    })
}

pub fn rewrite_prefix_in_query(query: &str, new_prefix: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping() -> PathMapping {
        PathMapping {
            bucket_segments: 2,
            bucket_separator: "--".to_string(),
            display_bucket_segments: 1,
        }
    }

    #[test]
    fn test_root_path() {
        assert_eq!(classify_request(&mapping(), "/", None), RequestClass::Index);
    }

    #[test]
    fn test_root_empty() {
        assert_eq!(classify_request(&mapping(), "", None), RequestClass::Index);
    }

    #[test]
    fn test_object_request() {
        let result = classify_request(&mapping(), "/cholmes/admin-boundaries/file.parquet", None);
        assert_eq!(
            result,
            RequestClass::ProxyRequest {
                rewritten_path: "/cholmes--admin-boundaries/file.parquet".to_string(),
                query: None,
            }
        );
    }

    #[test]
    fn test_object_request_nested_key() {
        let result =
            classify_request(&mapping(), "/cholmes/admin-boundaries/dir/subdir/file.parquet", None);
        assert_eq!(
            result,
            RequestClass::ProxyRequest {
                rewritten_path: "/cholmes--admin-boundaries/dir/subdir/file.parquet".to_string(),
                query: None,
            }
        );
    }

    #[test]
    fn test_product_list_via_segment() {
        let result = classify_request(
            &mapping(),
            "/cholmes/admin-boundaries",
            Some("list-type=2"),
        );
        assert_eq!(
            result,
            RequestClass::ProxyRequest {
                rewritten_path: "/cholmes--admin-boundaries".to_string(),
                query: Some("list-type=2".to_string()),
            }
        );
    }

    #[test]
    fn test_account_list() {
        let result = classify_request(&mapping(), "/cholmes", Some("list-type=2"));
        assert_eq!(
            result,
            RequestClass::AccountList {
                account: "cholmes".to_string(),
                query: Some("list-type=2".to_string()),
            }
        );
    }

    #[test]
    fn test_account_list_trailing_slash() {
        let result = classify_request(&mapping(), "/cholmes/", Some("list-type=2"));
        assert_eq!(
            result,
            RequestClass::AccountList {
                account: "cholmes".to_string(),
                query: Some("list-type=2".to_string()),
            }
        );
    }

    #[test]
    fn test_prefix_routed_list() {
        let result = classify_request(
            &mapping(),
            "/cholmes",
            Some("list-type=2&prefix=admin-boundaries/"),
        );
        assert_eq!(
            result,
            RequestClass::ProxyRequest {
                rewritten_path: "/cholmes--admin-boundaries".to_string(),
                query: Some("list-type=2&prefix=".to_string()),
            }
        );
    }

    #[test]
    fn test_prefix_routed_list_with_subdir() {
        let result = classify_request(
            &mapping(),
            "/cholmes",
            Some("list-type=2&prefix=admin-boundaries/subdir/"),
        );
        assert_eq!(
            result,
            RequestClass::ProxyRequest {
                rewritten_path: "/cholmes--admin-boundaries".to_string(),
                query: Some("list-type=2&prefix=subdir/".to_string()),
            }
        );
    }

    #[test]
    fn test_single_segment_no_list() {
        let result = classify_request(&mapping(), "/cholmes", None);
        assert_eq!(
            result,
            RequestClass::BadRequest("Missing product in path".to_string())
        );
    }

    #[test]
    fn test_url_encoded_prefix() {
        let result = classify_request(
            &mapping(),
            "/cholmes",
            Some("list-type=2&prefix=admin%20boundaries/subdir/"),
        );
        assert_eq!(
            result,
            RequestClass::ProxyRequest {
                rewritten_path: "/cholmes--admin boundaries".to_string(),
                query: Some("list-type=2&prefix=subdir/".to_string()),
            }
        );
    }

    // ── Query helper tests ──────────────────────────────────────────

    #[test]
    fn test_is_list_request() {
        assert!(is_list_request("list-type=2"));
        assert!(is_list_request("foo=bar&list-type=2&baz=qux"));
        assert!(!is_list_request("foo=bar"));
        assert!(!is_list_request(""));
    }

    #[test]
    fn test_extract_query_param() {
        assert_eq!(
            extract_query_param("list-type=2&prefix=foo/", "prefix"),
            Some("foo/".to_string())
        );
        assert_eq!(
            extract_query_param("list-type=2", "prefix"),
            None
        );
        assert_eq!(
            extract_query_param("prefix=hello%20world", "prefix"),
            Some("hello world".to_string())
        );
    }

    #[test]
    fn test_rewrite_prefix_in_query() {
        assert_eq!(
            rewrite_prefix_in_query("list-type=2&prefix=old/", "new/"),
            "list-type=2&prefix=new/"
        );
        assert_eq!(
            rewrite_prefix_in_query("prefix=old/&max-keys=100", ""),
            "prefix=&max-keys=100"
        );
    }
}
