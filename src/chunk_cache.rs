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
/// from the backend, bounding subrequest count and assembly memory. An
/// unaligned 32 MiB span touches at most 9 chunks × (match + put) + meta ≈ 20
/// Cache API ops, under the 50-per-request cap. Peak wasm memory ≈ the 32 MiB
/// assembly buffer plus one JS copy at response build; large concurrent cold
/// reads are the residency ceiling to watch against the 128 MB isolate limit.
// ponytail: fixed 32 MiB; lower it (or stream assembly) if isolate OOMs appear.
pub const MAX_CACHEABLE_SPAN: u64 = 32 * 1024 * 1024;

/// TTL for the per-object metadata entry (ETag + length). This is the staleness
/// window for *addressing* (which generation's chunks we look up), not for
/// correctness: chunk fetches carry `If-Match`, so a mid-read overwrite can
/// never stitch mixed generations — it just costs a bypass.
pub const META_TTL_SECS: u32 = 60;

/// Percent-encode set for one cache-key path segment: everything except RFC
/// 3986 unreserved. Same set as `source_api::cache::PATH_SEGMENT`; deliberately
/// duplicated rather than shared because `tests/chunk_cache.rs` compiles this
/// file standalone (`#[path]`) with no access to `crate::source_api`.
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
        (true, false) => digits(b).filter(|n| *n > 0).map(RangeSpec::Suffix),
        (false, true) => digits(a).map(RangeSpec::From),
        (false, false) => {
            let (s, e) = (digits(a)?, digits(b)?);
            (s <= e).then_some(RangeSpec::Bounded(s, e))
        }
        (true, true) => None,
    }
}

/// Parse an all-ASCII-digit offset. Unlike `u64::from_str`, rejects a leading
/// `+` (or any sign/whitespace), so a non-RFC-9110 `bytes=+0-+9` is refused and
/// bypasses instead of diverging from S3 (which ignores the malformed header).
fn digits(s: &str) -> Option<u64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
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
///
/// `headers` holds every origin entity header from the probe (content-type,
/// content-encoding, content-disposition, cache-control, last-modified,
/// x-amz-meta-*, …) so a cache-served response carries the same headers the
/// direct path would — minus the range framing (content-length/content-range),
/// which is regenerated per request. Dropping these silently corrupts, e.g.,
/// a `Content-Encoding: gzip` object served without the header.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ObjectMeta {
    pub etag: String,
    pub len: u64,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}

// ── Worker-side implementation (wasm only) ──────────────────────────

#[cfg(target_arch = "wasm32")]
pub use wasm_impl::{CachePlan, ChunkCachingBackend};

#[cfg(target_arch = "wasm32")]
mod wasm_impl {
    use super::*;
    use bytes::Bytes;
    use futures::StreamExt;
    use http::header::{
        HeaderName, HeaderValue, ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, ETAG, IF_MATCH,
        RANGE,
    };
    use http::HeaderMap;
    use multistore::backend::{ForwardResponse, ProxyBackend, RawResponse};
    use multistore::error::ProxyError;
    use multistore::route_handler::ForwardRequest;
    use multistore::types::BucketConfig;
    use multistore_cf_workers::{collect_js_body, JsBody, WorkerBackend};
    use object_store::list::PaginatedListStore;
    use object_store::signer::Signer;
    use std::rc::Rc;
    use std::sync::Arc;

    const X_CACHE: &str = "x-cache";
    const X_CACHE_CHUNKS: &str = "x-cache-chunks";

    /// Max concurrent in-flight chunk fetches in the streamed path. Kept small:
    /// each origin miss opens a subrequest, bounded by the 6-simultaneous-open-
    /// connection limit, and hits are edge-local anyway.
    const STREAM_CONCURRENCY: usize = 4;

    /// One chunk's owned work item for the streamed multi-chunk path. Owned (not
    /// borrowed) so the assembly stream satisfies `Response::from_stream`'s
    /// `'static` bound.
    struct ChunkJob {
        inner: WorkerBackend,
        plan: CachePlan,
        meta_key: String,
        etag: String,
        fetch_req: ForwardRequest,
        key: String,
        cb_start: u64,
        cb_end: u64,
        from: usize,
        to: usize,
    }

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
            // Any bypass — ineligible request or a mid-serve fallback — takes the
            // same path: forward the original request and stamp BYPASS. A bypass
            // is always a correct outcome (the origin serves the exact request).
            let outcome = if eligible(&request) {
                serve_chunked(plan, &self.inner, &request).await
            } else {
                Err("ineligible request")
            };
            match outcome {
                Ok(resp) => Ok(resp),
                Err(reason) => {
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

    /// The plan is built for object GETs only; here we additionally require a
    /// `Range` header and refuse conditional requests.
    ///
    /// Ranged-only is deliberate: cloud-native geospatial reads (COG tiles,
    /// GeoParquet footers, PMTiles directories, Zarr chunks) are all ranged and
    /// carry the cross-client reuse this cache exists to serve. Full-object GETs
    /// are bulk downloads (`aws s3 cp`, rclone) — throughput-bound, single-touch,
    /// and worst-case for the assemble-then-respond path — so they stay on the
    /// direct stream (and whole-object caching, if wanted, belongs in the native
    /// CDN cache, not here). Conditional requests keep their precondition
    /// semantics on the direct path. (`If-Range` is intentionally absent:
    /// multistore's GetObject allowlist never forwards it, so it can't reach us.)
    fn eligible(request: &ForwardRequest) -> bool {
        request.method == http::Method::GET
            && request.headers.contains_key(RANGE)
            && ![
                "if-match",
                "if-none-match",
                "if-modified-since",
                "if-unmodified-since",
            ]
            .iter()
            .any(|h| request.headers.contains_key(*h))
    }

    async fn serve_chunked(
        plan: &CachePlan,
        inner: &WorkerBackend,
        request: &ForwardRequest,
    ) -> Result<ForwardResponse<web_sys::Response>, &'static str> {
        // Eligibility guarantees a `Range` header; parse it (bypass anything the
        // aligned-chunk path can't serve: multi-range, non-bytes, garbage).
        let range_spec = request
            .headers
            .get(RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_range)
            .ok_or("unparseable or multi-range")?;
        // `bytes=0-` (open-ended from zero) is a full-object transfer wearing a
        // Range header — bypass it like a full-object GET (see `eligible`).
        if range_spec == RangeSpec::From(0) {
            return Err("full-object transfer (bytes=0-)");
        }

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
            return Err("empty object or non-strong etag");
        }

        // ── Resolve the client range against the object length ─────────
        // Unsatisfiable against cached meta → bypass, letting the origin
        // adjudicate against the *live* object. Synthesizing a 416 here would be
        // wrong for up to META_TTL_SECS after the object grew (the origin would
        // return 206), and would drop S3's parseable InvalidRange error body.
        let (start, end) = resolve_range(Some(&range_spec), meta.len)
            .ok_or("range unsatisfiable vs cached meta")?;
        if end - start + 1 > MAX_CACHEABLE_SPAN {
            return Err("span exceeds cacheable threshold");
        }

        let (first, last) = chunk_index_range(start, end, CHUNK_SIZE);
        let total_chunks = last - first + 1;

        // Single chunk (the common footer/tile read): buffer and respond, with
        // accurate HIT/MISS and no stream setup. A hit range-slices the Cache
        // API (fix #1) so only the requested bytes leave cache, not the 4 MiB
        // block.
        if first == last {
            let (cb_start, cb_end) = chunk_bounds(first, CHUNK_SIZE, meta.len);
            let key = chunk_key(&plan.prefix, &meta.etag, CHUNK_SIZE, first);
            let fetch_req = sub_request(
                request,
                &format!("bytes={cb_start}-{cb_end}"),
                Some(&meta.etag),
            );
            let (bytes, hit) = resolve_chunk(
                inner.clone(),
                plan.clone(),
                meta_key.clone(),
                meta.etag.clone(),
                fetch_req,
                key,
                cb_start,
                cb_end,
                (start - cb_start) as usize,
                (end - cb_start + 1) as usize,
            )
            .await?;
            return Ok(buffered_response(start, end, &meta, bytes, hit as u64, 1));
        }

        // Multi-chunk (large ranged read, e.g. a DuckDB column pull): stream the
        // assembly so bytes flush after the *first* chunk resolves instead of
        // after the whole span buffers (fix #2). `buffered` overlaps origin
        // misses at bounded concurrency and emits in order, so no chunk is
        // yielded before it's validated. Trade: once bytes have flushed, a
        // later-chunk failure can only truncate the response (client retry), not
        // bypass — correctness still holds via per-chunk length + ETag checks.
        let content_length = end - start + 1;
        let jobs: Vec<ChunkJob> = (first..=last)
            .map(|index| {
                let (cb_start, cb_end) = chunk_bounds(index, CHUNK_SIZE, meta.len);
                ChunkJob {
                    inner: inner.clone(),
                    plan: plan.clone(),
                    meta_key: meta_key.clone(),
                    etag: meta.etag.clone(),
                    fetch_req: sub_request(
                        request,
                        &format!("bytes={cb_start}-{cb_end}"),
                        Some(&meta.etag),
                    ),
                    key: chunk_key(&plan.prefix, &meta.etag, CHUNK_SIZE, index),
                    cb_start,
                    cb_end,
                    from: (start.max(cb_start) - cb_start) as usize,
                    to: (end.min(cb_end) - cb_start + 1) as usize,
                }
            })
            .collect();

        let stream = futures::stream::iter(jobs)
            .map(|job| async move {
                resolve_chunk(
                    job.inner,
                    job.plan,
                    job.meta_key,
                    job.etag,
                    job.fetch_req,
                    job.key,
                    job.cb_start,
                    job.cb_end,
                    job.from,
                    job.to,
                )
                .await
                .map(|(bytes, _hit)| bytes)
                .map_err(|e| worker::Error::RustError(e.to_string()))
            })
            .buffered(STREAM_CONCURRENCY);

        let ws: web_sys::Response = worker::Response::from_stream(stream)
            .map_err(|_| "stream build failed")?
            .into();
        Ok(streamed_response(
            start,
            end,
            &meta,
            content_length,
            ws,
            total_chunks,
        ))
    }

    /// Resolve one chunk to the client's slice `[from, to)` of it: a
    /// range-sliced Cache API hit, or a backend fetch of the full aligned chunk
    /// (validated, cached whole as prefetch, sliced for the response). Returns
    /// `(client_slice, was_hit)`. Owned params so it composes into the `'static`
    /// assembly stream.
    #[allow(clippy::too_many_arguments)]
    async fn resolve_chunk(
        inner: WorkerBackend,
        plan: CachePlan,
        meta_key: String,
        etag: String,
        fetch_req: ForwardRequest,
        key: String,
        cb_start: u64,
        cb_end: u64,
        from: usize,
        to: usize,
    ) -> Result<(Vec<u8>, bool), &'static str> {
        let cache = worker::Cache::default();
        let expected = cb_end - cb_start + 1;
        if let Some(bytes) = cache_get_chunk(&cache, &key, from, to, expected).await {
            return Ok((bytes, true));
        }
        // Miss: fetch the whole aligned chunk (the over-fetch is prefetch for
        // adjacent reads), validate its length, cache it whole, return only the
        // client's slice.
        let full = fetch_chunk(&inner, fetch_req, &cache, &meta_key, &etag).await?;
        if full.len() as u64 != expected {
            return Err("chunk length mismatch (backend)");
        }
        let slice = full[from..to].to_vec();
        background_put(
            &plan,
            key,
            full,
            "application/octet-stream",
            "public, max-age=31536000, immutable".to_string(),
        );
        Ok((slice, false))
    }

    /// Range-sliced Cache API lookup for one chunk: ask for just bytes
    /// `[from, to)` of the cached block. Cloudflare honors `Range` on a cached
    /// response and returns a server-side-sliced 206, so a warm read pulls the
    /// requested bytes rather than the whole 4 MiB block (fix #1); a runtime
    /// that ignores `Range` (e.g. miniflare) returns the full 200 and we slice
    /// in wasm. A cached chunk whose total length disagrees with `expected` is
    /// poison — evict it and report a miss so the caller re-fetches a clean copy.
    async fn cache_get_chunk(
        cache: &worker::Cache,
        key: &str,
        from: usize,
        to: usize,
        expected: u64,
    ) -> Option<Vec<u8>> {
        let req = build_range_request(key, from, to)?;
        let mut resp = cache.get(&req, false).await.ok().flatten()?;
        let want = to - from;
        match resp.status_code() {
            206 => {
                let total_ok = resp
                    .headers()
                    .get("content-range")
                    .ok()
                    .flatten()
                    .and_then(|v| parse_content_range_total(&v))
                    == Some(expected);
                if let Ok(bytes) = resp.bytes().await {
                    if total_ok && bytes.len() == want {
                        return Some(bytes);
                    }
                }
            }
            200 => {
                // Runtime ignored `Range` and returned the full body — slice here.
                if let Ok(bytes) = resp.bytes().await {
                    if bytes.len() as u64 == expected {
                        return Some(bytes[from..to].to_vec());
                    }
                }
            }
            _ => {} // 416 (chunk shorter than range) or odd status → evict below
        }
        let _ = cache.delete(key, false).await;
        None
    }

    /// Build a GET `Request` to the chunk key URL carrying
    /// `Range: bytes=from-(to-1)` (relative to the cached chunk body) for a
    /// range-sliced Cache API lookup.
    fn build_range_request(key: &str, from: usize, to: usize) -> Option<worker::Request> {
        let mut req = worker::Request::new(key, worker::Method::Get).ok()?;
        req.headers_mut()
            .ok()?
            .set("range", &format!("bytes={from}-{}", to - 1))
            .ok()?;
        Some(req)
    }

    /// Learn ETag + length with a 1-byte ranged GET. (HEAD would fail SigV4 —
    /// the backend URL is presigned for GET — and `Range` is unsigned, so this
    /// rides the same URL.)
    async fn probe_meta(
        inner: &WorkerBackend,
        request: &ForwardRequest,
    ) -> Result<ObjectMeta, &'static str> {
        let resp = inner
            .forward(sub_request(request, "bytes=0-0", None), JsBody::new(None))
            .await
            .map_err(|_| "meta probe fetch failed")?;
        if resp.status != 206 {
            // 200 = backend ignored Range; 416 = zero-length object; anything
            // else is the backend's problem to report on the direct path.
            return Err("meta probe not 206");
        }
        let etag = header_string(&resp.headers, ETAG.as_str())
            .filter(|e| is_strong_etag(e))
            .ok_or("missing or weak etag")?;
        let len = header_string(&resp.headers, CONTENT_RANGE.as_str())
            .as_deref()
            .and_then(parse_content_range_total)
            .ok_or("unparseable content-range")?;
        // Preserve every origin entity header for replay on cache-served
        // responses; skip only the range framing we regenerate per request.
        let headers = resp
            .headers
            .iter()
            .filter(|(n, _)| {
                let n = n.as_str();
                n != CONTENT_LENGTH.as_str() && n != CONTENT_RANGE.as_str()
            })
            .filter_map(|(n, v)| {
                v.to_str()
                    .ok()
                    .map(|v| (n.as_str().to_string(), v.to_string()))
            })
            .collect();
        Ok(ObjectMeta { etag, len, headers })
    }

    /// Fetch one aligned chunk from the backend via the pre-built ranged
    /// sub-request. Validates status + ETag (a drift means the object changed →
    /// evict meta and bypass, so mixed-generation stitching is impossible); the
    /// caller validates length and caches the chunk.
    async fn fetch_chunk(
        inner: &WorkerBackend,
        fetch_req: ForwardRequest,
        cache: &worker::Cache,
        meta_key: &str,
        etag: &str,
    ) -> Result<Vec<u8>, &'static str> {
        let resp = inner
            .forward(fetch_req, JsBody::new(None))
            .await
            .map_err(|_| "backend chunk fetch failed")?;

        let fresh = resp.status == 206
            && header_string(&resp.headers, ETAG.as_str()).as_deref() == Some(etag);
        if !fresh {
            let _ = cache.delete(meta_key, false).await;
            return Err("object changed under chunk fetch");
        }

        collect_js_body(JsBody::new(resp.body.body()))
            .await
            .map(|b| b.to_vec())
            .map_err(|_| "chunk body read failed")
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
        if let Ok(json) = serde_json::to_vec(meta) {
            background_put(
                plan,
                meta_key.to_string(),
                json,
                "application/json",
                format!("max-age={META_TTL_SECS}"),
            );
        }
    }

    /// Fire-and-forget cache write via `waitUntil` — never blocks or fails the
    /// client response; a failed put just costs a future re-fetch.
    fn background_put(
        plan: &CachePlan,
        key: String,
        body: Vec<u8>,
        content_type: &'static str,
        cache_control: String,
    ) {
        plan.ctx.wait_until(async move {
            let headers = worker::Headers::new();
            let _ = headers.set("content-type", content_type);
            let _ = headers.set("cache-control", &cache_control);
            if let Ok(resp) = worker::Response::from_bytes(body) {
                let _ = worker::Cache::default()
                    .put(key.as_str(), resp.with_headers(headers))
                    .await;
            }
        });
    }

    /// Headers shared by both response paths: replay the origin entity headers
    /// captured at probe time (content-type, content-encoding, etag,
    /// content-disposition, x-amz-meta-*, …) — one consistent generation,
    /// matching the direct path — then override range framing + cache signalling.
    fn cache_headers(
        start: u64,
        end: u64,
        meta: &ObjectMeta,
        content_length: u64,
        x_cache: &str,
        chunks: &str,
    ) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in &meta.headers {
            set_header(&mut headers, name, value);
        }
        set_header(&mut headers, ACCEPT_RANGES.as_str(), "bytes");
        set_header(
            &mut headers,
            CONTENT_LENGTH.as_str(),
            &content_length.to_string(),
        );
        set_header(&mut headers, X_CACHE, x_cache);
        set_header(&mut headers, X_CACHE_CHUNKS, chunks);
        set_header(
            &mut headers,
            CONTENT_RANGE.as_str(),
            &format!("bytes {start}-{end}/{}", meta.len),
        );
        headers
    }

    /// Single-chunk buffered 206. HIT/MISS is exact (one chunk, resolved before
    /// the response is built).
    fn buffered_response(
        start: u64,
        end: u64,
        meta: &ObjectMeta,
        body: Vec<u8>,
        hits: u64,
        total_chunks: u64,
    ) -> ForwardResponse<web_sys::Response> {
        let content_length = body.len() as u64;
        // `x-cache: HIT` is reserved for "every byte came from cache".
        let x_cache = if hits == total_chunks { "HIT" } else { "MISS" };
        let headers = cache_headers(
            start,
            end,
            meta,
            content_length,
            x_cache,
            &format!("{hits}/{total_chunks}"),
        );
        let uint8 = js_sys::Uint8Array::from(body.as_slice());
        let ws = web_sys::Response::new_with_opt_buffer_source(Some(&uint8))
            .unwrap_or_else(|_| web_sys::Response::new().unwrap());
        ForwardResponse {
            status: 206,
            headers,
            body: ws,
            content_length: Some(content_length),
        }
    }

    /// Multi-chunk streamed 206. Headers must flush before any chunk resolves,
    /// so per-chunk HIT/MISS can't be reported: `x-cache: MISS` is the
    /// conservative floor (never over-claims a hit) and `x-cache-chunks:
    /// stream/N` flags the path. Large multi-chunk reads are the minority;
    /// single-chunk reads (footers, tiles) keep exact HIT/MISS. Content-Length
    /// is exact — the client span is known up front — so the 206 framing is
    /// correct despite the body streaming.
    fn streamed_response(
        start: u64,
        end: u64,
        meta: &ObjectMeta,
        content_length: u64,
        body: web_sys::Response,
        total_chunks: u64,
    ) -> ForwardResponse<web_sys::Response> {
        let headers = cache_headers(
            start,
            end,
            meta,
            content_length,
            "MISS",
            &format!("stream/{total_chunks}"),
        );
        ForwardResponse {
            status: 206,
            headers,
            body,
            content_length: Some(content_length),
        }
    }

    // ── Small helpers ───────────────────────────────────────────────

    /// Insert a header from string name + value, silently skipping any that
    /// won't parse (so a malformed stored value can't fail the whole response).
    fn set_header(headers: &mut HeaderMap, name: &str, value: &str) {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(n, v);
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
}
