# ADR-009: Chunk-Aligned Edge Caching of Object Bytes

**Date:** 2026-07-19
**RFC:** RFC-001 §9 (outbound storage)
**Depends on:** ADR-006
**Issue / PR:** [#188](https://github.com/source-cooperative/data.source.coop/issues/188) / [#189](https://github.com/source-cooperative/data.source.coop/pull/189)
**Code:** `src/chunk_cache.rs` (mechanism), `src/lib.rs` (the `chunk_plan` gate), `src/analytics.rs` (`blob10`)

---

## Context

The proxy serves S3-style object reads for Source Cooperative. Traffic is cloud-native geospatial — COG, GeoParquet, PMTiles, Zarr — read over HTTP **range requests** by GDAL/`vsicurl`, titiler/rio-tiler, DuckDB, pyarrow, and browser clients (deck.gl / MapLibre). The dominant pattern is **many clients re-reading overlapping hot regions of the same objects** (a parquet footer, a COG header/overviews, popular map tiles), not one client scanning a file once.

Every such read hit the origin object store. We wanted an edge cache to (a) offload the origin and (b) cut latency on repeated reads, under hard constraints from #188:

1. Authorization must run on **every** request (hit or miss).
2. Per-product analytics must see every request, plus a cache-hit dimension.
3. Ranged GETs dominate, and the Cloudflare Cache API **cannot store 206 responses**.
4. Objects can be multi-GB and can be overwritten in place.
5. The cache key must **never** include auth material.

---

## Decision

Serve **ranged, public** object GETs through a per-PoP chunk cache built on the Cloudflare Cache API, wired as a `ProxyBackend` wrapper (`ChunkCachingBackend`) around multistore's `WorkerBackend` (see ADR-006). The gateway authorizes first; the wrapper only decides whether the *backend fetch* may ride the cache. A global `CHUNK_CACHE_ENABLED` flag and a fail-closed gate (`chunk_plan` in `lib.rs`) mean any error, non-public product, or ineligible request yields `plan = None` and the wrapper delegates straight to the backend — an always-exercised "cache off" path.

### 4 MiB aligned chunks

The client range is normalized to fixed 4 MiB blocks; each block is looked up in the Cache API and fetched from the backend on miss as a ranged GET, then stored as a **200** (the Cache API refuses 206s) with an immutable TTL. Fixed alignment is what makes overlapping-but-unequal ranges (`bytes=-8` vs `bytes=-100`) collide onto the *same* cached entry — the property that turns a shared hot region into cross-client reuse.

### Ranged-only, public-only

Full-object GETs and `bytes=0-` open-ended transfers bypass to the direct stream — they are bulk, single-touch, and the worst case for assemble-then-respond; whole-object caching, if ever wanted, belongs in the native CDN cache rather than this sub-object layer. Spans larger than 32 MiB also bypass, bounding subrequests and assembly memory. Private products never cache.

### Range-sliced hits

A warm read asks the Cache API for just the requested bytes of a cached block (`cache.get` with a `Range` header → Cloudflare returns a server-side-sliced 206), with an in-wasm slice fallback for runtimes that ignore `Range`. The chunk stays 4 MiB so a **cold** miss still over-fetches the whole block — this is deliberate read-ahead for adjacent tiles/blocks (COG grids, `vsicurl` scans); only the **warm** read-out, which has no prefetch value, is trimmed.

### Streamed multi-chunk assembly

A range spanning more than one chunk is assembled through a bounded-concurrency ordered stream (`Response::from_stream`) so the first bytes flush after the first chunk resolves instead of after the whole span buffers. Single-chunk reads keep a simple buffered path with exact hit accounting.

### The presigning property that makes it free

GETs are presigned as query-string SigV4 with `Range` **unsigned and pass-through**, so one presigned URL serves every chunk sub-fetch with a different `Range` header — no re-signing, no multistore change, and authz / analytics / error mapping stay byte-identical.

### Keys carry content identity only

`https://{host}/.chunk-cache/v1/{account}/{product}/{key}?etag=…&cs=…&i=N` — derived from the decoded client path, never the presigned backend URL. ETag in the key makes overwrites self-invalidating; chunk size (`cs`) makes tuning self-invalidating; auth material never appears.

---

## Eviction & Consistency Under Overwrite

There is no global purge; chunks leave the cache four ways:

1. **ETag-in-key rotation (primary, passive).** New content → new ETag → new keys. Old chunks are orphaned (not deleted) and reclaimed by LRU. A content change needs no active eviction.
2. **TTL + LRU.** Chunks are stored `immutable` (~1 y) — effectively permanent because they are immutable *by content* — until per-PoP LRU pressure evicts them.
3. **Explicit `cache.delete`** for poison (a cached chunk whose length disagrees with the object meta) and for ETag drift (deletes the meta entry to force a re-probe). Current-PoP only, only for keys a request computes.
4. **Cloudflare zone purge-by-URL** — possible externally, not wired up; ETag keying makes it unnecessary for correctness.

Per-object metadata (ETag + length + entity headers) is learned via a 1-byte probe and cached for `META_TTL_SECS` (60 s); every chunk fetch carries `If-Match`. **When an upstream object changes:** the first chunk **miss** against it sends `If-Match: {old_etag}`, gets a 412, deletes the meta entry, and **bypasses** that request to origin; the next request re-probes, learns the new ETag, and populates a new generation under new keys.

- **Correctness is always intact** — mixed-generation stitching is structurally impossible (chunks of different generations live under different keys, and an `If-Match` drift aborts before any wrong byte is emitted; length validation catches wrong-sized chunks).
- **Freshness is eventually consistent, bounded by `META_TTL_SECS` (60 s) per PoP.** Within that window a read served entirely from still-warm old-generation hits returns the old content consistently (no miss fires, so no 412). Lowering `META_TTL_SECS` shrinks the window at the cost of more probes / origin load.
- This relies on backends honoring HTTP ETag semantics; we gate on **strong** ETags (weak/missing bypass). A backend that reused an ETag for different content would defeat it.

---

## Observability & Rollout

- `x-cache: HIT | MISS | BYPASS` and `x-cache-chunks` on responses. Single-chunk reads report exact `hits/total`; multi-chunk **streamed** reads report `stream/N` with `x-cache: MISS` — headers must flush before chunks resolve, so per-chunk precision isn't available there (a conservative floor; the majority small footer/tile reads are single-chunk and reported exactly).
- Analytics gains an **append-only** `blob10 = cache_status`, normalized to the exact tokens so a CDN-fronted backend's own `x-cache` cannot pollute the taxonomy.
- Staging-first: `CHUNK_CACHE_ENABLED` on for staging + PR previews, off in prod until a soak looks good. Kill switch = flip it back.

---

## Consequences

**Positive**

- Origin offload on repeated reads (~8× less origin work on a hit; warm `server-timing: backend` collapses to single digits), shared across every client at a PoP.
- Warm ranged-read latency **beats** origin (range-sliced hits: ~1.8–2.9× faster TTFB on a 64 KiB read); the real geopandas windowed read is at parity, up from 0.7× before the range-slice/stream work.
- No multistore changes; a clean, always-exercised bypass path.

**Negative / accepted trade-offs**

- Large-range **throughput** is parity-to-slightly-below origin on a fast network (the cache adds a re-serve hop). Least-relevant axis for ranged workloads; true bulk transfers bypass entirely.
- **Eventual consistency** on overwrite, bounded by the 60 s meta TTL per PoP.
- Streamed multi-chunk responses can't report per-chunk hit precision and, on a post-flush chunk failure, truncate (client retry) rather than bypass — correctness still holds via per-chunk length + ETag validation.
- Cache memory is per-PoP and LRU-governed, not directly controllable.

---

## Alternatives Considered

- **Cache whole objects / full-object GETs** — rejected: bulk, low-reuse, worst-case TTFB; belongs in the native CDN cache, not this sub-object layer.
- **Smaller chunks to cut read amplification** — rejected: forfeits the cold over-fetch's prefetch value for tiled formats (COG / PMTiles / `vsicurl`). We attacked warm read-out amplification with range-sliced hits instead, keeping 4 MiB.
- **Per-range (non-aligned) caching** — rejected: overlapping-but-unequal ranges wouldn't share an entry, collapsing reuse.
- **Request-shape bypass of small unaligned cold reads** — deferred: would forfeit prefetch and can't distinguish isolated-cold from first-hot-tile.

---

## Future Options

- **Per-product opt-in.** The gate already holds the resolved `SourceProduct`, so making caching conditional is one predicate. Cheapest is an env-var allow/deny list (proxy-only); self-service would be a product flag from the Source API — the same mechanism as the existing `disabled` / `visibility` bits, requiring a control-plane schema field. See PR #189 discussion.
- HEAD responses on a short-TTL cache (the meta entry already exists to build on).
- Private products (authz already runs per request; needs a cache-key hygiene review first).
- Request collapsing for cold-object bursts (same class as #148).
