//! Default `Cache-Control` for proxied responses.
//!
//! Kept wasm-free so the policy can be unit-tested natively (see
//! `tests/cache_control.rs`), despite the crate's `[lib] test = false`.
//!
//! Without a freshness policy, RFC 9111 §4.2.2 lets caches invent one from
//! `Last-Modified`, which served pre-publish STAC metadata to browsers (#225).
//! This sets what downstream caches see; it does not make ranged reads
//! edge-cacheable (#188).

/// `Cache-Control` for `/.sts`. Its 200 body is temporary AWS credentials, and a
/// `GET` with the parameters in the query string is a valid STS call, so the
/// response must never be stored — not even by the caller's own browser.
pub(crate) const STS_CACHE_CONTROL: &str = "no-store";

/// `Cache-Control` for a non-public product whose backend sets no policy:
/// shared caches must not keep its bytes, whoever fetched them, and `no-cache`
/// keeps browsers from applying heuristic freshness. Applied even when the
/// configured default is disabled.
pub(crate) const PRIVATE_CACHE_CONTROL: &str = "private, no-cache";

/// multistore-cf-workers appends this to every forwarded response so Cloudflare
/// doesn't recompress object bodies. It says nothing about freshness, so it is
/// ignored when deciding whether the backend set a policy, and always kept.
const NO_TRANSFORM: &str = "no-transform";

/// Decide the `Cache-Control` to set on a response, or `None` to leave it as is.
///
/// * `/.sts` is always [`STS_CACHE_CONTROL`]: it returns credentials.
/// * Writes and `304`s are left alone. A cache merges a 304's headers into
///   the stored response ([RFC 9111 §4.3.4]), so setting one there would
///   overwrite the publisher's directive from the original 200.
/// * A non-public product (`private`) is always `private`: the backend's
///   directives are kept minus `public` and `s-maxage`, or
///   [`PRIVATE_CACHE_CONTROL`] if it set none.
/// * Anything else keeps a backend `Cache-Control` or `Expires` untouched
///   (the publisher wins), and otherwise gets `configured` (`None` disables
///   it). That covers every read status — heuristic freshness applies to 404s
///   too — and `/.well-known/*`, whose JWKS must not outlive a key rotation.
///
/// [RFC 9111 §4.3.4]: https://www.rfc-editor.org/rfc/rfc9111#section-4.3.4
pub(crate) fn default_cache_control(
    method: &http::Method,
    path: &str,
    status: u16,
    backend_cache_control: Option<&str>,
    backend_expires: bool,
    private: bool,
    configured: Option<&str>,
) -> Option<String> {
    if path == "/.sts" {
        return Some(STS_CACHE_CONTROL.to_string());
    }
    if !matches!(*method, http::Method::GET | http::Method::HEAD) || status == 304 {
        return None;
    }
    let backend = backend_cache_control.unwrap_or_default();
    let directives: Vec<&str> = backend
        .split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty() && !d.eq_ignore_ascii_case(NO_TRANSFORM))
        .collect();
    let has_policy = backend_expires || !directives.is_empty();

    let mut value = if private {
        let kept: Vec<&str> = directives
            .into_iter()
            .filter(|d| {
                let name = d.split('=').next().unwrap_or_default().trim();
                !["public", "s-maxage", "private"]
                    .iter()
                    .any(|n| name.eq_ignore_ascii_case(n))
            })
            .collect();
        if has_policy {
            std::iter::once("private")
                .chain(kept)
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            PRIVATE_CACHE_CONTROL.to_string()
        }
    } else if has_policy {
        return None;
    } else {
        configured?.to_string()
    };

    let had_no_transform = backend
        .split(',')
        .any(|d| d.trim().eq_ignore_ascii_case(NO_TRANSFORM));
    if had_no_transform {
        value.push_str(", ");
        value.push_str(NO_TRANSFORM);
    }
    Some(value)
}
