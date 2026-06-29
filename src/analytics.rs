#[cfg(target_arch = "wasm32")]
use worker::{AnalyticsEngineDataPointBuilder, Env};

#[cfg(target_arch = "wasm32")]
use crate::header_str;

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Metadata collected from the request and response for analytics logging.
pub struct RequestEvent<'a> {
    pub account_id: &'a str,
    pub product_id: &'a str,
    pub file_path: &'a str,
    pub method: &'a str,
    pub user_id: &'a str,
    /// HMAC-SHA256 of the client IP — see [`hash_ip`]. We never log the raw
    /// IP; this lets us count distinct clients without storing PII.
    pub client_ip_hash: &'a str,
    /// `Range` request header with the `bytes=` unit prefix stripped
    /// (e.g. `0-1023`), empty if absent.
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
    ///   blob8: client_ip_hash (HMAC-SHA256; empty when IP is unknown)
    ///   blob9: range (Range header, "bytes=" prefix stripped, empty if absent)
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

/// HMAC-SHA256 of a client IP keyed by `salt`, hex-encoded (64 chars).
///
/// `salt` is a deployment secret; without it the small IPv4 space could be
/// brute-forced back to raw IPs. HMAC (rather than a bare `SHA256(salt ‖ ip)`)
/// is the conventional keyed-hash primitive — robust regardless of how this
/// output is later reused. Empty `ip` in → empty out, so anonymous or unknown
/// clients stay empty rather than all collapsing to one hash.
pub fn hash_ip(ip: &str, salt: &str) -> String {
    if ip.is_empty() {
        return String::new();
    }
    let mut mac =
        Hmac::<Sha256>::new_from_slice(salt.as_bytes()).expect("HMAC accepts any key length");
    mac.update(ip.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Strip the constant `bytes=` unit prefix from a `Range` header value
/// (e.g. `bytes=0-1023` → `0-1023`) so blob9 stores just the parseable ranges.
/// A non-bytes unit (legal per RFC 7233, never seen in practice) and the empty
/// string pass through unchanged, so nothing is silently mangled.
pub fn strip_range_unit(range: &str) -> &str {
    range.strip_prefix("bytes=").unwrap_or(range)
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

/// Build a `RequestEvent` from the request + response and write it to the
/// Analytics Engine. Never errors — failures are logged and swallowed.
#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn log_analytics(
    env: &Env,
    headers: &http::HeaderMap,
    response: &web_sys::Response,
    method: &http::Method,
    account: Option<&str>,
    product: Option<&str>,
    key: Option<&str>,
    duration_ms: f64,
    ip_hash_salt: &str,
) {
    let content_type = response
        .headers()
        .get("content-type")
        .ok()
        .flatten()
        .unwrap_or_default();
    let bytes_sent: f64 = response
        .headers()
        .get("content-length")
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);

    let client_ip_hash = hash_ip(header_str(headers, "cf-connecting-ip"), ip_hash_salt);
    let range = strip_range_unit(header_str(headers, "range"));

    log_request(
        env,
        &RequestEvent {
            account_id: account.unwrap_or(""),
            product_id: product.unwrap_or(""),
            file_path: key.unwrap_or(""),
            method: method.as_str(),
            user_id: header_str(headers, "x-source-user-id"),
            client_ip_hash: &client_ip_hash,
            range,
            country: header_str(headers, "cf-ipcountry"),
            content_type: &content_type,
            bytes_sent,
            status_code: response.status() as f64,
            duration_ms,
        },
    );
}
