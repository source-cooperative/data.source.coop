use worker::{AnalyticsEngineDataPointBuilder, Env};

/// Metadata collected from the request and response for analytics logging.
pub struct RequestEvent<'a> {
    pub account_id: &'a str,
    pub product_id: &'a str,
    pub file_path: &'a str,
    pub method: &'a str,
    pub user_id: &'a str,
    pub country: &'a str,
    pub content_type: &'a str,
    pub bytes_sent: f64,
    pub status_code: f64,
}

/// Write a request event to the Analytics Engine dataset.
///
/// Schema:
///   index1:  "{account_id}/{product_id}" (sampling boundary per product)
///   blob1:   account_id
///   blob2:   product_id
///   blob3:   file_path (truncated to 256 bytes)
///   blob4:   method
///   blob5:   user_id
///   blob6:   country
///   blob7:   content_type
///   double1: bytes_sent
///   double2: status_code
///
/// This function never returns an error — failures are logged and swallowed
/// so that analytics never blocks a response.
pub fn log_request(env: &Env, event: &RequestEvent) {
    let dataset = match env.analytics_engine("ANALYTICS") {
        Ok(ds) => ds,
        Err(e) => {
            tracing::warn!("analytics engine binding unavailable: {e:?}");
            return;
        }
    };

    // Truncate file_path to 256 bytes (Analytics Engine blob limit)
    let file_path = truncate_to_byte_limit(event.file_path, 256);

    let index = format!("{}/{}", event.account_id, event.product_id);

    let result = AnalyticsEngineDataPointBuilder::new()
        .indexes([index.as_str()])
        .add_blob(event.account_id) // blob1
        .add_blob(event.product_id) // blob2
        .add_blob(file_path) // blob3
        .add_blob(event.method) // blob4
        .add_blob(event.user_id) // blob5
        .add_blob(event.country) // blob6
        .add_blob(event.content_type) // blob7
        .add_double(event.bytes_sent) // double1
        .add_double(event.status_code) // double2
        .write_to(&dataset);

    if let Err(e) = result {
        tracing::warn!("failed to write analytics data point: {e:?}");
    }
}

/// Extract account and product segments from a URL path.
///
/// Given `/{account}/{product}[/{key}]`, returns `(account, product, key)`.
/// Returns `None` for segments that aren't present.
pub fn extract_path_segments(path: &str) -> (Option<&str>, Option<&str>, Option<&str>) {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return (None, None, None);
    }
    let mut parts = trimmed.splitn(3, '/');
    let account = parts.next();
    let product = parts.next();
    let key = parts.next();
    (account, product, key)
}

/// Truncate a string to at most `max_bytes` bytes on a char boundary.
fn truncate_to_byte_limit(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
