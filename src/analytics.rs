#[cfg(target_arch = "wasm32")]
use worker::{AnalyticsEngineDataPointBuilder, Env};

#[cfg(target_arch = "wasm32")]
use crate::header_str;

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;

/// Metadata collected from the request and response for analytics logging.
pub struct RequestEvent<'a> {
    pub account_id: &'a str,
    pub product_id: &'a str,
    pub file_path: &'a str,
    pub method: &'a str,
    /// The principal the request's signature verified as: a person's Ory
    /// identity ID, or the service account an API key or platform token acts
    /// as. Empty when anonymous.
    pub user_id: &'a str,
    /// The access key ID that signed the request; with the `credentials
    /// issued` log line, it names the API key or platform subject behind an
    /// account principal. Empty when anonymous.
    pub access_key_id: &'a str,
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

    /// Blob columns in Analytics Engine schema order (blob1..blob10).
    ///
    ///   blob1: account_id
    ///   blob2: product_id
    ///   blob3: file_path (truncated to 256 bytes)
    ///   blob4: method
    ///   blob5: user_id (verified principal; empty for anonymous requests)
    ///   blob6: country
    ///   blob7: content_type
    ///   blob8: client_ip_hash (HMAC-SHA256; empty when IP is unknown)
    ///   blob9: range (Range header, "bytes=" prefix stripped, empty if absent)
    ///   blob10: access_key_id (empty for anonymous requests)
    pub fn blobs(&self) -> [&str; 10] {
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
            self.access_key_id,
        ]
    }

    /// Double columns in Analytics Engine schema order (double1..double4).
    ///
    ///   double1: bytes_sent
    ///   double2: status_code
    ///   double3: duration_ms
    ///   double4: sample weight — how many requests this row stands for (1 when
    ///            kept unsampled). Rows written before it existed read as 0, so
    ///            queries use `greatest(double4, 1)`.
    pub fn doubles(&self, weight: f64) -> [f64; 4] {
        [self.bytes_sent, self.status_code, self.duration_ms, weight]
    }

    /// Events that are never sampled: every error but 404, and every signed
    /// read. These are rare, and they are the ones read one by one (an outage,
    /// who read a restricted product), so a sample would lose what they are for.
    /// 404s are excluded because missing keys are routine for some products
    /// (a client polling for a file that is not published yet can make them
    /// most of a product's traffic). Writes are signed too but are not exempt:
    /// a bulk upload or delete is exactly the volume this exists to bound.
    pub fn is_always_kept(&self) -> bool {
        let is_read = matches!(self.method, "GET" | "HEAD");
        (self.status_code >= 400.0 && self.status_code != 404.0)
            || (is_read && !self.user_id.is_empty())
    }
}

/// How requests are sampled before being written to Analytics Engine.
///
/// Workers Analytics Engine bills each data point written, whether or not it is
/// stored, so reducing the bill means writing fewer of them. (Its own
/// adaptive sampling thins stored rows only.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleConfig {
    /// Past the burst allowance, one request in `every` is written, with weight
    /// `every`. 1 turns sampling off.
    pub every: u32,
    /// Requests per product, per isolate, per minute that are always written.
    pub burst: u32,
}

impl SampleConfig {
    /// Read from the `ANALYTICS_SAMPLE_EVERY` and `ANALYTICS_SAMPLE_BURST`
    /// values. Unset, empty or unparseable means no sampling; unset burst
    /// means 5. A zero `every` is treated as 1.
    pub fn parse(every: Option<&str>, burst: Option<&str>) -> Self {
        let num = |v: Option<&str>| v.and_then(|s| s.trim().parse::<u32>().ok());
        SampleConfig {
            every: num(every).unwrap_or(1).max(1),
            burst: num(burst).unwrap_or(5),
        }
    }
}

/// Length of the window the burst allowance is counted over.
const SAMPLE_WINDOW_MS: u64 = 60_000;
/// Products tracked at once. Past this the table is cleared, which only
/// restores a fresh burst allowance; nothing is lost.
const MAX_TRACKED_PRODUCTS: usize = 2_048;

/// Decides which request events are written, and with what weight.
///
/// The first `burst` requests for a product in each window, per isolate, are
/// written at weight 1. Past that, one in `every` is written at weight `every`.
/// Either way the weights sum to the true request count in expectation, so
/// `SUM(sampleInterval * greatest(double4, 1))` is an unbiased request count.
/// A quiet product never leaves its burst allowance and is recorded in full; a
/// hot one is thinned. That keeps the long tail visible (ADR-008 rejected
/// sampling by request for starving it).
///
/// The state is per isolate and so per datacenter, with no coordination: how
/// much a hot product is thinned depends on how many isolates serve it.
pub struct Sampler {
    cfg: SampleConfig,
    windows: HashMap<String, (u64, u32)>,
}

impl Sampler {
    pub fn new(cfg: SampleConfig) -> Self {
        Sampler {
            cfg,
            windows: HashMap::new(),
        }
    }

    /// `None` drops the event; `Some(weight)` writes it. `now_ms` is the
    /// wall-clock time and `u` a uniform random number in `[0, 1)`; both are
    /// passed in so the decision is deterministic under test.
    pub fn decide(&mut self, event: &RequestEvent, now_ms: u64, u: f64) -> Option<f64> {
        if self.cfg.every <= 1 || event.is_always_kept() {
            return Some(1.0);
        }

        let key = event.index();
        if self.windows.len() >= MAX_TRACKED_PRODUCTS && !self.windows.contains_key(&key) {
            self.windows.clear();
        }
        let (start, count) = self.windows.entry(key).or_insert((now_ms, 0));
        // A clock that steps backwards also starts a new window.
        if now_ms < *start || now_ms - *start >= SAMPLE_WINDOW_MS {
            *start = now_ms;
            *count = 0;
        }
        *count = count.saturating_add(1);

        if *count <= self.cfg.burst {
            Some(1.0)
        } else if u * (self.cfg.every as f64) < 1.0 {
            Some(self.cfg.every as f64)
        } else {
            None
        }
    }
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    /// One sampler per isolate, built from the environment on first use.
    static SAMPLER: std::cell::RefCell<Option<Sampler>> = const { std::cell::RefCell::new(None) };
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

    let weight = SAMPLER.with(|cell| {
        cell.borrow_mut()
            .get_or_insert_with(|| {
                let var = |name: &str| env.var(name).ok().map(|v| v.to_string());
                Sampler::new(SampleConfig::parse(
                    var("ANALYTICS_SAMPLE_EVERY").as_deref(),
                    var("ANALYTICS_SAMPLE_BURST").as_deref(),
                ))
            })
            .decide(event, js_sys::Date::now() as u64, js_sys::Math::random())
    });
    let Some(weight) = weight else {
        return;
    };

    let index = event.index();
    let mut builder = AnalyticsEngineDataPointBuilder::new().indexes([index.as_str()]);
    for blob in event.blobs() {
        builder = builder.add_blob(blob);
    }
    for double in event.doubles(weight) {
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

/// The access key ID a SigV4 `Authorization` header was signed with, parsed by
/// the same function multistore verified the signature with. The header itself
/// is not signed, so any other reading of it could disagree with the key the
/// signature was checked against. Empty when the header does not parse.
///
/// A presigned URL never reaches here: multistore authenticates only the
/// `Authorization` header, so a presigned request has no principal.
pub fn access_key_id(authorization: &str) -> String {
    multistore::auth::parse_sigv4_auth(authorization)
        .map(|sig| sig.access_key_id)
        .unwrap_or_default()
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
    principal: Option<&str>,
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
    // Only once the principal is known: before that, the header is just what
    // the client sent, verified by nothing.
    let signing_key = principal
        .map(|_| access_key_id(header_str(headers, "authorization")))
        .unwrap_or_default();

    log_request(
        env,
        &RequestEvent {
            account_id: account.unwrap_or(""),
            product_id: product.unwrap_or(""),
            file_path: key.unwrap_or(""),
            method: method.as_str(),
            user_id: principal.unwrap_or(""),
            access_key_id: &signing_key,
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
