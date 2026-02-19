//! XML rewriting for S3 list responses.
//!
//! When a backend prefix is configured, the backend returns keys that include
//! the prefix. This module strips that prefix and optionally prepends a new
//! one, so clients see the expected key structure.

use crate::resolver::ListRewrite;

/// Rewrite `<Key>` and `<Prefix>` element values in a ListObjectsV2 XML response
/// according to the given [`ListRewrite`] rule.
pub fn rewrite_list_response(xml: &str, rewrite: &ListRewrite) -> String {
    let mut result = xml.to_string();
    result = rewrite_xml_element_values(&result, "Key", &rewrite.strip_prefix, &rewrite.add_prefix);
    result = rewrite_xml_element_values(&result, "Prefix", &rewrite.strip_prefix, &rewrite.add_prefix);
    result
}

/// Replace prefix in XML element values:
/// `<Tag>old_prefix/rest</Tag>` -> `<Tag>new_prefix/rest</Tag>`
fn rewrite_xml_element_values(
    xml: &str,
    tag: &str,
    old_prefix: &str,
    new_prefix: &str,
) -> String {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let mut result = String::with_capacity(xml.len());
    let mut remaining = xml;

    while let Some(start_idx) = remaining.find(&open) {
        result.push_str(&remaining[..start_idx + open.len()]);
        remaining = &remaining[start_idx + open.len()..];

        if let Some(end_idx) = remaining.find(&close) {
            let value = &remaining[..end_idx];
            if let Some(stripped) = value.strip_prefix(old_prefix) {
                if new_prefix.is_empty() {
                    result.push_str(stripped.trim_start_matches('/'));
                } else {
                    result.push_str(new_prefix);
                    if !stripped.is_empty() && !stripped.starts_with('/') {
                        result.push('/');
                    }
                    result.push_str(stripped.trim_start_matches('/'));
                }
            } else {
                result.push_str(value);
            }
            result.push_str(&close);
            remaining = &remaining[end_idx + close.len()..];
        } else {
            // Malformed XML — just append the rest
            break;
        }
    }
    result.push_str(remaining);
    result
}

/// Extract the text content of the first occurrence of `<tag>...</tag>`.
pub fn extract_xml_element<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(&xml[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rewrite_strips_prefix() {
        let xml = r#"<ListBucketResult><Contents><Key>base/mirror/file.csv</Key></Contents></ListBucketResult>"#;
        let rewrite = ListRewrite {
            strip_prefix: "base/mirror/".to_string(),
            add_prefix: "repo".to_string(),
        };
        let result = rewrite_list_response(xml, &rewrite);
        assert!(result.contains("<Key>repo/file.csv</Key>"), "got: {}", result);
    }

    #[test]
    fn test_rewrite_strip_only() {
        let xml = r#"<ListBucketResult><Contents><Key>prefix/file.csv</Key></Contents></ListBucketResult>"#;
        let rewrite = ListRewrite {
            strip_prefix: "prefix/".to_string(),
            add_prefix: String::new(),
        };
        let result = rewrite_list_response(xml, &rewrite);
        assert!(result.contains("<Key>file.csv</Key>"));
    }

    #[test]
    fn test_extract_xml_element() {
        let xml = r#"<Prefix>some/prefix/</Prefix>"#;
        assert_eq!(extract_xml_element(xml, "Prefix"), Some("some/prefix/"));
    }

    #[test]
    fn test_no_match_preserves_xml() {
        let xml = r#"<Key>other/file.csv</Key>"#;
        let rewrite = ListRewrite {
            strip_prefix: "nonexistent/".to_string(),
            add_prefix: "new/".to_string(),
        };
        let result = rewrite_list_response(xml, &rewrite);
        assert!(result.contains("<Key>other/file.csv</Key>"));
    }
}
