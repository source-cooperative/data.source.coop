//! Cross-isolate (per-colo) L2 cache for the STS `AssumeRoleWithWebIdentity`
//! response, layered UNDER multistore's in-isolate credential cache.
//!
//! The slow part of backend federation is the STS round-trip, and multistore's
//! credential cache lives in per-isolate memory — so every cold isolate re-runs
//! it on the request hot path. Caching the STS response in the Cloudflare Cache
//! API (shared across isolates within a colo) means only one isolate per colo
//! per credential lifetime actually calls STS; the rest serve the cached body.
//!
//! Only the pure helpers live here so they are host-testable via
//! `tests/sts_cache.rs` (the lib is `cdylib` with `test = false`). The Cache API
//! I/O lives in `lib.rs`, where the wasm-only `worker` types are available.

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

/// Stop serving a cached response this many seconds before the credential
/// actually expires. Kept >= multistore's own 60s in-isolate refresh lead, so an
/// L2 entry always expires before L1 would consider the derived credential stale
/// — the two tiers never hand out an about-to-expire credential.
pub const REFRESH_LEAD_SECS: i64 = 300; // 5 minutes

/// The `RoleArn` iff `form` is an `AssumeRoleWithWebIdentity` request — the only
/// exchange we cache (other STS actions and the Azure/GCP bearer flows return
/// `None` and bypass the cache). `RoleArn` is multistore's own L1 cache key, so
/// L2 keys line up with L1 exactly.
pub fn role_arn_from_form<'a>(form: &'a [(&'a str, &'a str)]) -> Option<&'a str> {
    let is_assume_role = form
        .iter()
        .any(|&(k, v)| k == "Action" && v == "AssumeRoleWithWebIdentity");
    if !is_assume_role {
        return None;
    }
    form.iter().find(|&&(k, _)| k == "RoleArn").map(|&(_, v)| v)
}

/// Cache key: a synthetic, non-routable URL. Cache API entries are only returned
/// when a request URL matches the key, and this host never arrives as a real edge
/// request — so a cached (short-lived, role-scoped) credential is not externally
/// addressable.
pub fn cache_key(role_arn: &str) -> String {
    format!(
        "https://sts-creds.cache.internal/v1/{}",
        utf8_percent_encode(role_arn, NON_ALPHANUMERIC)
    )
}

/// Seconds the STS response may be cached: time until its `<Expiration>` minus
/// the refresh lead. `None` means **do not cache** — either an STS error document
/// (no parseable `<Expiration>`) or a response already inside the lead window.
/// `now_unix` is injected (rather than read from a clock) so this stays pure and
/// host-testable.
pub fn ttl_secs(sts_response_xml: &str, now_unix: i64) -> Option<u32> {
    let exp = extract_tag(sts_response_xml, "Expiration")?;
    let exp_unix = chrono::DateTime::parse_from_rfc3339(exp.trim())
        .ok()?
        .timestamp();
    let ttl = exp_unix - now_unix - REFRESH_LEAD_SECS;
    (ttl > 0).then_some(ttl as u32)
}

/// First-match text of `<tag>…</tag>`. The STS body is a fixed, trusted shape, so
/// a full XML parser is not warranted.
fn extract_tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(&xml[start..end])
}
