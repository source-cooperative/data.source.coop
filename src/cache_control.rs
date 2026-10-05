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

/// `Cache-Control` for credentialed reads, used in place of the configured
/// default. A restricted product's bytes must not land in a shared cache, and
/// RFC 9111 §3.5 only keeps them out on the `Authorization` header — not for a
/// presigned URL, and not at all once the operator's value says `public`.
/// `no-cache` too, so heuristic freshness can't apply in the browser either.
pub(crate) const CREDENTIALED_CACHE_CONTROL: &str = "private, no-cache";

/// Whether the request carries caller credentials: an `Authorization` header
/// (SigV4 or bearer) or a SigV4 presigned query string.
pub(crate) fn is_credentialed(headers: &http::HeaderMap, query: Option<&str>) -> bool {
    headers.contains_key(http::header::AUTHORIZATION)
        || query.is_some_and(|q| {
            q.split('&').any(|pair| {
                let key = pair.split('=').next().unwrap_or_default();
                key.eq_ignore_ascii_case("X-Amz-Signature")
            })
        })
}

/// Decide the `Cache-Control` value to add to a response, or `None` to leave the
/// response untouched.
///
/// `/.sts` always gets [`STS_CACHE_CONTROL`], whatever the method, status or
/// configuration. Otherwise a value is returned only when all of the following
/// hold:
///
/// * `configured` is `Some`. `build_config` maps an empty or all-whitespace
///   `DEFAULT_CACHE_CONTROL` to `None`, which disables the default entirely.
/// * The request is a read (`GET`/`HEAD`). Responses to writes are not
///   heuristically cacheable and gain nothing from the header.
/// * The response is not a `304 Not Modified`. Per [RFC 9111 §4.3.4] a cache
///   *updates the stored response's headers* from the 304, so injecting here
///   would overwrite a publisher's `max-age=31536000, immutable` — stored from
///   the original 200 — with our default on every revalidation.
/// * The backend set no freshness policy of its own: no non-blank
///   `Cache-Control` and no `Expires`. **Passthrough always wins**: a
///   publisher's `max-age=31536000, immutable`, or an `Expires` date, is what
///   they asked for, and adding `no-cache` would override either. A blank
///   `Cache-Control` carries no directive, so it counts as absent.
///
/// A credentialed request gets [`CREDENTIALED_CACHE_CONTROL`] in place of the
/// configured value, so an operator's `public, max-age=…` can never make a
/// restricted product's bytes storable by a shared cache. A publisher's own
/// header still wins on credentialed reads too: if they mark a restricted
/// object `public`, a shared cache may store it.
///
/// Applied to every read status, not just 200: RFC 9111 §4.2.2 heuristic
/// freshness also covers 206, 404 and 410. Also applied to `/.well-known/*`:
/// `multistore-oidc-provider` serves discovery and JWKS with no
/// `Cache-Control`, and a JWKS cached past an `OIDC_PROVIDER_KID_PREVIOUS`
/// rotation rejects tokens signed by the new key.
///
/// [RFC 9111 §4.3.4]: https://www.rfc-editor.org/rfc/rfc9111#section-4.3.4
pub(crate) fn default_cache_control<'a>(
    method: &http::Method,
    path: &str,
    status: u16,
    backend_cache_control: Option<&str>,
    backend_expires: bool,
    credentialed: bool,
    configured: Option<&'a str>,
) -> Option<&'a str> {
    if path == "/.sts" {
        return Some(STS_CACHE_CONTROL);
    }
    let configured = configured?;
    if !matches!(*method, http::Method::GET | http::Method::HEAD) || status == 304 {
        return None;
    }
    if backend_expires || backend_cache_control.is_some_and(|v| !v.trim().is_empty()) {
        return None;
    }
    Some(if credentialed {
        CREDENTIALED_CACHE_CONTROL
    } else {
        configured
    })
}
