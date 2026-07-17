//! Chunk-aligned edge caching of object bytes via the Cache API (issue #188).
//!
//! Object GETs on public products are served through a per-PoP chunk cache:
//! the requested range is normalized to fixed-size aligned blocks, each block
//! is looked up in the Cache API and fetched from the backend on miss (as a
//! ranged GET against the same presigned URL — `Range` is not part of the
//! SigV4 query signature), and the client's exact range is assembled from the
//! blocks. Blocks are keyed by content identity only (client path + ETag +
//! chunk index) — never by auth material — and the ETag in the key makes
//! overwrites self-invalidating.
//!
//! The wiring lives in [`ChunkCachingBackend`] (wasm-only, bottom of file),
//! which wraps multistore's `WorkerBackend` and is reached only *after* the
//! gateway has authorized the request. Everything above it is pure range /
//! key math, unit-tested natively in `tests/chunk_cache.rs`.

use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

/// Fixed block size for cached chunks. Baked into the cache key (`cs=`), so
/// changing it self-invalidates old entries rather than corrupting them.
// ponytail: fixed 4 MiB; promote to an env var if hit-rate data demands tuning.
pub const CHUNK_SIZE: u64 = 4 * 1024 * 1024;

/// Requested spans larger than this bypass the chunk cache and stream straight
/// from the backend, bounding subrequest count and assembly memory (32 MiB
/// buffered worst-case, well under the 128 MB isolate limit; ≤ 8 chunks ×
/// (match + put) + meta ≈ 18 Cache API ops against the 50-per-request cap).
pub const MAX_CACHEABLE_SPAN: u64 = 32 * 1024 * 1024;

/// TTL for the per-object metadata entry (ETag + length). This is the staleness
/// window for *addressing* (which generation's chunks we look up), not for
/// correctness: chunk fetches carry `If-Match`, so a mid-read overwrite can
/// never stitch mixed generations — it just costs a bypass.
pub const META_TTL_SECS: u32 = 60;

/// Percent-encode set for one cache-key path segment: everything except RFC
/// 3986 unreserved. Same set as the Source API cache keys — slugs pass through
/// byte-identical while `?`, `#`, `&`, `/` etc. can't inject or collide.
const KEY_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

// ── Range parsing / resolution ──────────────────────────────────────

/// A parsed single-range `Range` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSpec {
    /// `bytes=s-e` (inclusive).
    Bounded(u64, u64),
    /// `bytes=s-` (from offset to EOF).
    From(u64),
    /// `bytes=-n` (last n bytes).
    Suffix(u64),
}

/// Parse a `Range` header value. `None` for anything the chunk path shouldn't
/// touch: multi-range, non-`bytes` units, `bytes=-0`, inverted bounds, garbage.
pub fn parse_range(value: &str) -> Option<RangeSpec> {
    let spec = value.strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None; // multi-range: rare, and unservable from aligned chunks
    }
    let (a, b) = spec.split_once('-')?;
    match (a.is_empty(), b.is_empty()) {
        (true, false) => b.parse().ok().filter(|n| *n > 0).map(RangeSpec::Suffix),
        (false, true) => a.parse().ok().map(RangeSpec::From),
        (false, false) => {
            let (s, e) = (a.parse().ok()?, b.parse().ok()?);
            (s <= e).then_some(RangeSpec::Bounded(s, e))
        }
        (true, true) => None,
    }
}

/// Resolve a parsed range against the object length into inclusive `[start,
/// end]` byte offsets. `spec = None` means "no Range header" → the full object.
/// Returns `None` when the range is unsatisfiable (start beyond EOF → 416).
/// `len` must be > 0 (zero-length objects bypass the chunk path entirely).
pub fn resolve_range(spec: Option<&RangeSpec>, len: u64) -> Option<(u64, u64)> {
    match spec {
        None => Some((0, len - 1)),
        Some(RangeSpec::Bounded(s, e)) => (*s < len).then(|| (*s, (*e).min(len - 1))),
        Some(RangeSpec::From(s)) => (*s < len).then(|| (*s, len - 1)),
        Some(RangeSpec::Suffix(n)) => Some((len.saturating_sub(*n), len - 1)),
    }
}

// ── Chunk math ──────────────────────────────────────────────────────

/// Indices of the first and last chunk covering `[start, end]` (inclusive).
pub fn chunk_index_range(start: u64, end: u64, chunk_size: u64) -> (u64, u64) {
    (start / chunk_size, end / chunk_size)
}

/// Inclusive byte bounds of chunk `index` within an object of length `len`
/// (the last chunk is trimmed to EOF).
pub fn chunk_bounds(index: u64, chunk_size: u64, len: u64) -> (u64, u64) {
    let start = index * chunk_size;
    (start, (start + chunk_size).min(len) - 1)
}

// ── Cache keys ──────────────────────────────────────────────────────

/// Cache-key prefix for one object, derived from the worker host and the
/// *decoded client path* (`/{account}/{product}/{key}`) — never from the
/// presigned backend URL, whose query carries auth material. Each path
/// segment is percent-encoded (injectively) and `/` structure is preserved,
/// so a trailing-slash key stays distinct from its slashless sibling.
/// `/.chunk-cache/` sits in the proxy's reserved non-product namespace.
pub fn object_cache_prefix(host: &str, client_path: &str) -> String {
    let encoded: Vec<String> = client_path
        .trim_start_matches('/')
        .split('/')
        .map(|seg| utf8_percent_encode(seg, KEY_SEGMENT).to_string())
        .collect();
    format!("https://{host}/.chunk-cache/v1/{}", encoded.join("/"))
}

/// Cache key for the per-object metadata entry.
pub fn meta_key(prefix: &str) -> String {
    format!("{prefix}?meta")
}

/// Cache key for one chunk of one generation of an object. ETag in the key
/// makes overwrites self-invalidating; chunk size in the key makes a future
/// `CHUNK_SIZE` change self-invalidating.
pub fn chunk_key(prefix: &str, etag: &str, chunk_size: u64, index: u64) -> String {
    format!(
        "{prefix}?etag={}&cs={chunk_size}&i={index}",
        utf8_percent_encode(etag, KEY_SEGMENT)
    )
}

// ── Backend response parsing ────────────────────────────────────────

/// Total object length from a `Content-Range: bytes s-e/total` value.
/// `None` for `*` totals or anything malformed.
pub fn parse_content_range_total(value: &str) -> Option<u64> {
    value
        .strip_prefix("bytes ")?
        .split_once('/')?
        .1
        .parse()
        .ok()
}

/// Only strong ETags may key chunks: weak ETags (`W/"..."`) don't guarantee
/// byte-for-byte identity, which chunk stitching requires.
pub fn is_strong_etag(etag: &str) -> bool {
    etag.len() >= 2 && etag.starts_with('"') && etag.ends_with('"')
}

/// Per-object metadata cached under [`meta_key`] for [`META_TTL_SECS`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ObjectMeta {
    pub etag: String,
    pub len: u64,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
}

// ── Worker-side implementation (wasm only) ──────────────────────────

#[cfg(target_arch = "wasm32")]
pub use wasm_impl::{CachePlan, ChunkCachingBackend};

#[cfg(target_arch = "wasm32")]
mod wasm_impl {
    use super::*;
    use bytes::Bytes;
    use http::header::{
        HeaderValue, ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, IF_MATCH,
        LAST_MODIFIED, RANGE,
    };
    use http::HeaderMap;
    use multistore::backend::{ForwardResponse, ProxyBackend, RawResponse};
    use multistore::error::ProxyError;
    use multistore::route_handler::ForwardRequest;
    use multistore::types::BucketConfig;
    use multistore_cf_workers::{JsBody, WorkerBackend};
    use object_store::list::PaginatedListStore;
    use object_store::signer::Signer;
    use std::rc::Rc;
    use std::sync::Arc;

    const X_CACHE: &str = "x-cache";
    const X_CACHE_CHUNKS: &str = "x-cache-chunks";

    /// Everything the chunk cache needs that the backend call can't see:
    /// content identity from the client path, and the fetch event context for
    /// background `cache.put`s. Built in `lib.rs` only when the request is an
    /// object GET on a product whose *anonymous* view is public — so private
    /// bytes can never enter or leave the cache, and gateway authorization has
    /// already run by the time `forward()` consults the plan.
    #[derive(Clone)]
    pub struct CachePlan {
        prefix: String,
        ctx: Rc<worker::Context>,
    }

    impl CachePlan {
        pub fn new(host: &str, client_path: &str, ctx: Rc<worker::Context>) -> Self {
            Self {
                prefix: object_cache_prefix(host, client_path),
                ctx,
            }
        }
    }

    /// [`ProxyBackend`] wrapper that serves eligible object GETs through the
    /// chunk cache and delegates everything else to [`WorkerBackend`].
    #[derive(Clone)]
    pub struct ChunkCachingBackend {
        inner: WorkerBackend,
        plan: Option<CachePlan>,
    }

    impl ChunkCachingBackend {
        pub fn new(inner: WorkerBackend, plan: Option<CachePlan>) -> Self {
            Self { inner, plan }
        }
    }

    /// Outcome of the chunk path short of a fully assembled response.
    enum ServeError {
        /// Serve the original request directly from the backend instead
        /// (stamped `x-cache: BYPASS`). Always a correct outcome.
        Fallback(&'static str),
        /// A locally synthesized terminal response (e.g. 416).
        Respond(ForwardResponse<web_sys::Response>),
    }

    impl ProxyBackend for ChunkCachingBackend {
        type ResponseBody = web_sys::Response;
        type Body = JsBody;

        async fn forward(
            &self,
            request: ForwardRequest,
            body: JsBody,
        ) -> Result<ForwardResponse<web_sys::Response>, ProxyError> {
            let Some(plan) = self.plan.as_ref() else {
                return self.inner.forward(request, body).await;
            };
            if !eligible(&request) {
                let mut resp = self.inner.forward(request, body).await?;
                resp.headers
                    .insert(X_CACHE, HeaderValue::from_static("BYPASS"));
                return Ok(resp);
            }
            match serve_chunked(plan, &self.inner, &request).await {
                Ok(resp) => Ok(resp),
                Err(ServeError::Respond(resp)) => Ok(resp),
                Err(ServeError::Fallback(reason)) => {
                    tracing::debug!(reason, "chunk cache bypass");
                    let mut resp = self.inner.forward(request, body).await?;
                    resp.headers
                        .insert(X_CACHE, HeaderValue::from_static("BYPASS"));
                    Ok(resp)
                }
            }
        }

        fn create_paginated_store(
            &self,
            config: &BucketConfig,
        ) -> Result<Box<dyn PaginatedListStore>, ProxyError> {
            self.inner.create_paginated_store(config)
        }

        fn create_signer(&self, config: &BucketConfig) -> Result<Arc<dyn Signer>, ProxyError> {
            self.inner.create_signer(config)
        }

        async fn send_raw(
            &self,
            method: http::Method,
            url: String,
            headers: HeaderMap,
            body: Bytes,
        ) -> Result<RawResponse, ProxyError> {
            self.inner.send_raw(method, url, headers, body).await
        }
    }

    /// The plan is built for object GETs only; here we additionally refuse
    /// conditional requests (v1 keeps their semantics on the direct path —
    /// a mismatched `If-Range` must become a full 200, etc.).
    fn eligible(request: &ForwardRequest) -> bool {
        request.method == http::Method::GET
            && ![
                "if-match",
                "if-none-match",
                "if-modified-since",
                "if-unmodified-since",
                "if-range",
            ]
            .iter()
            .any(|h| request.headers.contains_key(*h))
    }

    async fn serve_chunked(
        plan: &CachePlan,
        inner: &WorkerBackend,
        request: &ForwardRequest,
    ) -> Result<ForwardResponse<web_sys::Response>, ServeError> {
        let range_spec = match request.headers.get(RANGE) {
            Some(v) => Some(
                v.to_str()
                    .ok()
                    .and_then(parse_range)
                    .ok_or(ServeError::Fallback("unparseable or multi-range"))?,
            ),
            None => None,
        };

        let cache = worker::Cache::default();
        let meta_key = meta_key(&plan.prefix);

        // ── Object metadata: ETag + length (cached, else 1-byte probe) ──
        let meta = match cache_get_json::<ObjectMeta>(&cache, &meta_key).await {
            Some(meta) => meta,
            None => {
                let meta = probe_meta(inner, request).await?;
                put_meta(plan, &meta_key, &meta);
                meta
            }
        };
        if meta.len == 0 || !is_strong_etag(&meta.etag) {
            return Err(ServeError::Fallback("empty object or non-strong etag"));
        }

        // ── Resolve the client range against the object length ─────────
        let Some((start, end)) = resolve_range(range_spec.as_ref(), meta.len) else {
            return Err(ServeError::Respond(range_not_satisfiable(meta.len)));
        };
        if end - start + 1 > MAX_CACHEABLE_SPAN {
            return Err(ServeError::Fallback("span exceeds cacheable threshold"));
        }

        // ── Gather chunks: cache hit or backend ranged GET ──────────────
        // ponytail: sequential chunk fetch; parallelize if spans grow beyond
        // the couple-of-chunks windowed reads this is built for.
        let (first, last) = chunk_index_range(start, end, CHUNK_SIZE);
        let mut body = Vec::with_capacity((end - start + 1) as usize);
        let total_chunks = last - first + 1;
        let mut hits = 0u64;
        for index in first..=last {
            let key = chunk_key(&plan.prefix, &meta.etag, CHUNK_SIZE, index);
            let (cb_start, cb_end) = chunk_bounds(index, CHUNK_SIZE, meta.len);
            let bytes = match cache.get(key.as_str(), false).await.ok().flatten() {
                Some(mut hit) => {
                    hits += 1;
                    hit.bytes()
                        .await
                        .map_err(|_| ServeError::Fallback("cached chunk read failed"))?
                }
                None => {
                    fetch_chunk(
                        plan, inner, request, &cache, &meta_key, &key, &meta, cb_start, cb_end,
                    )
                    .await?
                }
            };
            if bytes.len() as u64 != cb_end - cb_start + 1 {
                return Err(ServeError::Fallback("chunk length mismatch"));
            }
            let from = start.max(cb_start) - cb_start;
            let to = end.min(cb_end) - cb_start + 1;
            body.extend_from_slice(&bytes[from as usize..to as usize]);
        }

        Ok(build_response(
            range_spec.is_some(),
            start,
            end,
            &meta,
            body,
            hits,
            total_chunks,
        ))
    }

    /// Learn ETag + length with a 1-byte ranged GET. (HEAD would fail SigV4 —
    /// the backend URL is presigned for GET — and `Range` is unsigned, so this
    /// rides the same URL.)
    async fn probe_meta(
        inner: &WorkerBackend,
        request: &ForwardRequest,
    ) -> Result<ObjectMeta, ServeError> {
        let resp = inner
            .forward(sub_request(request, "bytes=0-0", None), JsBody::new(None))
            .await
            .map_err(|_| ServeError::Fallback("meta probe fetch failed"))?;
        if resp.status != 206 {
            // 200 = backend ignored Range; 416 = zero-length object; anything
            // else is the backend's problem to report on the direct path.
            return Err(ServeError::Fallback("meta probe not 206"));
        }
        let etag = header_string(&resp.headers, ETAG.as_str())
            .filter(|e| is_strong_etag(e))
            .ok_or(ServeError::Fallback("missing or weak etag"))?;
        let len = header_string(&resp.headers, CONTENT_RANGE.as_str())
            .as_deref()
            .and_then(parse_content_range_total)
            .ok_or(ServeError::Fallback("unparseable content-range"))?;
        Ok(ObjectMeta {
            etag,
            len,
            content_type: header_string(&resp.headers, CONTENT_TYPE.as_str()),
            last_modified: header_string(&resp.headers, LAST_MODIFIED.as_str()),
        })
    }

    /// Fetch one aligned chunk from the backend and cache it in the background.
    #[allow(clippy::too_many_arguments)]
    async fn fetch_chunk(
        plan: &CachePlan,
        inner: &WorkerBackend,
        request: &ForwardRequest,
        cache: &worker::Cache,
        meta_key: &str,
        key: &str,
        meta: &ObjectMeta,
        cb_start: u64,
        cb_end: u64,
    ) -> Result<Vec<u8>, ServeError> {
        let range = format!("bytes={cb_start}-{cb_end}");
        let resp = inner
            .forward(
                sub_request(request, &range, Some(&meta.etag)),
                JsBody::new(None),
            )
            .await
            .map_err(|_| ServeError::Fallback("backend chunk fetch failed"))?;

        // 412 (If-Match) or an ETag drift means the object was overwritten
        // since the meta entry was written: drop the meta so the next request
        // re-probes, and serve this one directly — mixed-generation stitching
        // is structurally impossible.
        let fresh = resp.status == 206
            && header_string(&resp.headers, ETAG.as_str()).as_deref() == Some(meta.etag.as_str());
        if !fresh {
            let _ = cache.delete(meta_key, false).await;
            return Err(ServeError::Fallback("object changed under chunk fetch"));
        }

        let bytes = response_bytes(resp.body)
            .await
            .map_err(|_| ServeError::Fallback("chunk body read failed"))?;

        // Background put — never blocks the client response, and put failures
        // only cost a future re-fetch. Entries are immutable by construction
        // (ETag + chunk size in the key), hence the year-long TTL.
        let put_key = key.to_string();
        let put_bytes = bytes.clone();
        plan.ctx.wait_until(async move {
            let headers = worker::Headers::new();
            let _ = headers.set("cache-control", "public, max-age=31536000, immutable");
            let _ = headers.set("content-type", "application/octet-stream");
            if let Ok(resp) = worker::Response::from_bytes(put_bytes) {
                let _ = worker::Cache::default()
                    .put(put_key.as_str(), resp.with_headers(headers))
                    .await;
            }
        });

        Ok(bytes)
    }

    /// Clone the (already allowlist-filtered) forward request with our own
    /// `Range` and optional `If-Match`, against the same presigned URL.
    fn sub_request(
        request: &ForwardRequest,
        range: &str,
        if_match: Option<&str>,
    ) -> ForwardRequest {
        let mut headers = request.headers.clone();
        if let Ok(v) = HeaderValue::from_str(range) {
            headers.insert(RANGE, v);
        }
        match if_match.and_then(|e| HeaderValue::from_str(e).ok()) {
            Some(v) => {
                headers.insert(IF_MATCH, v);
            }
            None => {
                headers.remove(IF_MATCH);
            }
        }
        ForwardRequest {
            method: request.method.clone(),
            url: request.url.clone(),
            headers,
            request_id: request.request_id.clone(),
        }
    }

    fn put_meta(plan: &CachePlan, meta_key: &str, meta: &ObjectMeta) {
        let (key, json) = (meta_key.to_string(), serde_json::to_string(meta));
        plan.ctx.wait_until(async move {
            let Ok(json) = json else { return };
            let headers = worker::Headers::new();
            let _ = headers.set("content-type", "application/json");
            let _ = headers.set("cache-control", &format!("max-age={META_TTL_SECS}"));
            if let Ok(resp) = worker::Response::ok(json) {
                let _ = worker::Cache::default()
                    .put(key.as_str(), resp.with_headers(headers))
                    .await;
            }
        });
    }

    fn build_response(
        is_range: bool,
        start: u64,
        end: u64,
        meta: &ObjectMeta,
        body: Vec<u8>,
        hits: u64,
        total_chunks: u64,
    ) -> ForwardResponse<web_sys::Response> {
        let mut headers = HeaderMap::new();
        // All headers come from the meta entry — one consistent generation.
        set_header(&mut headers, ETAG.as_str(), &meta.etag);
        set_header(&mut headers, ACCEPT_RANGES.as_str(), "bytes");
        if let Some(ct) = &meta.content_type {
            set_header(&mut headers, CONTENT_TYPE.as_str(), ct);
        }
        if let Some(lm) = &meta.last_modified {
            set_header(&mut headers, LAST_MODIFIED.as_str(), lm);
        }
        set_header(
            &mut headers,
            CONTENT_LENGTH.as_str(),
            &body.len().to_string(),
        );
        // `x-cache: HIT` is reserved for "every byte came from cache".
        let status_label = if hits == total_chunks { "HIT" } else { "MISS" };
        set_header(&mut headers, X_CACHE, status_label);
        set_header(
            &mut headers,
            X_CACHE_CHUNKS,
            &format!("{hits}/{total_chunks}"),
        );
        let status = if is_range {
            set_header(
                &mut headers,
                CONTENT_RANGE.as_str(),
                &format!("bytes {start}-{end}/{}", meta.len),
            );
            206
        } else {
            200
        };

        let content_length = body.len() as u64;
        let uint8 = js_sys::Uint8Array::from(body.as_slice());
        let ws = web_sys::Response::new_with_opt_buffer_source(Some(&uint8))
            .unwrap_or_else(|_| web_sys::Response::new().unwrap());
        ForwardResponse {
            status,
            headers,
            body: ws,
            content_length: Some(content_length),
        }
    }

    /// Local 416 — the meta entry alone proves the range is unsatisfiable, so
    /// no backend round-trip is spent confirming it.
    fn range_not_satisfiable(len: u64) -> ForwardResponse<web_sys::Response> {
        let mut headers = HeaderMap::new();
        set_header(
            &mut headers,
            CONTENT_RANGE.as_str(),
            &format!("bytes */{len}"),
        );
        set_header(&mut headers, X_CACHE, "HIT");
        ForwardResponse {
            status: 416,
            headers,
            body: web_sys::Response::new().unwrap(),
            content_length: Some(0),
        }
    }

    // ── Small helpers ───────────────────────────────────────────────

    fn set_header(headers: &mut HeaderMap, name: &'static str, value: &str) {
        if let Ok(v) = HeaderValue::from_str(value) {
            headers.insert(name, v);
        }
    }

    fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    }

    async fn cache_get_json<T: serde::de::DeserializeOwned>(
        cache: &worker::Cache,
        key: &str,
    ) -> Option<T> {
        let mut resp = cache.get(key, false).await.ok().flatten()?;
        let text = resp.text().await.ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Buffer a backend response body (≤ one chunk) via `arrayBuffer()`.
    async fn response_bytes(resp: web_sys::Response) -> Result<Vec<u8>, ()> {
        let promise = resp.array_buffer().map_err(|_| ())?;
        let buf = wasm_bindgen_futures::JsFuture::from(promise)
            .await
            .map_err(|_| ())?;
        Ok(js_sys::Uint8Array::new(&buf).to_vec())
    }
}
