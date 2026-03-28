//! S3-compliant redirect support for renamed accounts.

use serde::{Deserialize, Serialize};

/// Redirect info returned by the Source API when an account has been renamed.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RedirectInfo {
    pub redirect_to: String,
}

/// Build a redirect path by replacing the first path segment (account) with `new_account`.
///
/// Preserves the product, key, and query string.
/// Input `path` is the original decoded path (e.g. `/old-account/product/key`).
pub fn build_redirect_path(path: &str, query: Option<&str>, new_account: &str) -> String {
    let trimmed = path.strip_prefix('/').unwrap_or(path);
    let rest = match trimmed.find('/') {
        Some(idx) => &trimmed[idx..],
        None => "",
    };

    let new_path = format!("/{}{}", new_account, rest);

    match query {
        Some(q) if !q.is_empty() => format!("{}?{}", new_path, q),
        _ => new_path,
    }
}

/// Generate an S3 PermanentRedirect XML error body.
pub fn permanent_redirect_xml(new_account: &str, request_id: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Error>
<Code>PermanentRedirect</Code>
<Message>The account you are attempting to access must be addressed using the specified endpoint.</Message>
<Endpoint>data.source.coop</Endpoint>
<Bucket>{}</Bucket>
<RequestId>{}</RequestId>
</Error>"#,
        new_account, request_id
    )
}

/// Extract account and optionally product from the raw request path.
///
/// Returns `(Some(account), Some(product))` for `/{account}/{product}[/...]`
/// and `(Some(account), None)` for `/{account}[/]`.
pub fn extract_redirect_segments(path: &str) -> (Option<&str>, Option<&str>) {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        return (None, None);
    }
    let mut parts = trimmed.splitn(3, '/');
    let account = parts.next().filter(|s| !s.is_empty());
    let product = parts.next().filter(|s| !s.is_empty());
    (account, product)
}
