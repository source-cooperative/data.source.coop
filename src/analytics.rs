#[cfg(target_arch = "wasm32")]
use worker::{AnalyticsEngineDataPointBuilder, Env};

use sha2::{Digest, Sha256};

/// Metadata collected from the request and response for analytics logging.
pub struct RequestEvent<'a> {
    pub account_id: &'a str,
    pub product_id: &'a str,
    pub file_path: &'a str,
    pub method: &'a str,
    pub user_id: &'a str,
    /// Salted SHA-256 of the client IP — see [`hash_ip`]. We never log the raw
    /// IP; this lets us count distinct clients without storing PII.
    pub client_ip_hash: &'a str,
    /// `Range` request header verbatim (e.g. `bytes=0-1023`), empty if absent.
    pub range: &'a str,
    pub country: &'a str,
    pub content_type: &'a str,
    pub bytes_sent: f64,
    pub status_code: f64,
    pub duration_ms: f64,
}

impl RequestEvent<'_> {
    /// Sampling index: "{account_id}/{product_id}" (sampling boundary per product).
    pub fn index(&self) -> String {
        format!("{}/{}", self.account_id, self.product_id)
    }

    /// Blob columns in Analytics Engine schema order (blob1..blob9).
    ///
    ///   blob1: account_id
    ///   blob2: product_id
    ///   blob3: file_path (truncated to 256 bytes)
    ///   blob4: method
    ///   blob5: user_id (empty for anonymous requests)
    ///   blob6: country
    ///   blob7: content_type
    ///   blob8: client_ip_hash (salted SHA-256; empty when IP is unknown)
    ///   blob9: range (Range request header, empty if absent)
    pub fn blobs(&self) -> [&str; 9] {
        [
            self.account_id,
            self.product_id,
            // Truncate file_path to 256 bytes (Analytics Engine blob limit)
            truncate_to_byte_limit(self.file_path, 256),
            self.method,
            self.user_id,
            self.country,
            self.content_type,
            self.client_ip_hash,
            self.range,
        ]
    }

    /// Double columns in Analytics Engine schema order (double1..double3).
    ///
    ///   double1: bytes_sent
    ///   double2: status_code
    ///   double3: duration_ms
    pub fn doubles(&self) -> [f64; 3] {
        [self.bytes_sent, self.status_code, self.duration_ms]
    }
}

/// Write a request event to the Analytics Engine dataset.
///
/// See [`RequestEvent::blobs`] and [`RequestEvent::doubles`] for the schema.
///
/// This function never returns an error — failures are logged and swallowed
/// so that analytics never blocks a response.
#[cfg(target_arch = "wasm32")]
pub fn log_request(env: &Env, event: &RequestEvent) {
    let dataset = match env.analytics_engine("ANALYTICS") {
        Ok(ds) => ds,
        Err(e) => {
            tracing::warn!("analytics engine binding unavailable: {e:?}");
            return;
        }
    };

    let index = event.index();
    let mut builder = AnalyticsEngineDataPointBuilder::new().indexes([index.as_str()]);
    for blob in event.blobs() {
        builder = builder.add_blob(blob);
    }
    for double in event.doubles() {
        builder = builder.add_double(double);
    }

    if let Err(e) = builder.write_to(&dataset) {
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

/// Salted SHA-256 of a client IP, hex-encoded (64 chars).
///
/// `salt` is a deployment secret; without it the small IPv4 space could be
/// brute-forced back to raw IPs. Empty `ip` in → empty out, so anonymous or
/// unknown clients stay empty rather than all collapsing to one hash.
pub fn hash_ip(ip: &str, salt: &str) -> String {
    if ip.is_empty() {
        return String::new();
    }
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(ip.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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
