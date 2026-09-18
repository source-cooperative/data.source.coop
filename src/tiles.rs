//! PMTiles Z/X/Y tile endpoint.
//!
//! Serves `…/{archive}.pmtiles/{z}/{x}/{y}.{ext}` and `…/{archive}.pmtiles/tiles.json`
//! by resolving tiles out of a PMTiles v3 archive that already lives in a
//! product, and caching each tile at the edge.
//!
//! # Why this exists
//!
//! A map client reading a `.pmtiles` archive directly issues one HTTP range
//! request per tile plus directory reads, and none of them can be cached: the
//! Cache API refuses to store a `206`, and multistore deliberately keeps ranged
//! subrequests out of the shared cache
//! (`ForwardRequest::should_bypass_cache`) so a partial body can never poison
//! the full-object entry. Every tile therefore costs a round trip to origin
//! storage, for every user, forever. A tile is a small, whole, `200`-able
//! resource, so re-shaping the request is what makes it cacheable at all.
//!
//! It also buys Z/X/Y compatibility: clients that cannot read PMTiles — older
//! MapLibre and Leaflet builds, QGIS XYZ layers, anything that takes a tile URL
//! template — can consume a Source Cooperative tileset for the first time.
//!
//! # Tiles are decompressed here, not relayed compressed
//!
//! Archives store tiles compressed (gzip, for every archive seen so far), and
//! relaying those bytes with `Content-Encoding: gzip` looks like free
//! throughput — skip a decompress per tile inside WASM, hand the client what it
//! wanted on the wire anyway. It does not work. The runtime applies its own
//! transfer compression on top of a body that already carries an explicit
//! `Content-Encoding`, and the Cache API adds another layer across a
//! put/get round trip. Measured against wrangler, a tile came back
//! *triple*-gzipped: 17880 bytes on the wire unwrapping to 17852, then 17824,
//! then finally 26885 bytes of actual MVT. Clients decode one layer and get
//! gzip bytes labelled as a vector tile.
//!
//! So tiles are decompressed in the worker and served plain, letting the
//! runtime negotiate `Content-Encoding` the way it wants to. Tiles are small
//! (tens of KB) and a decompress only happens on a cache miss, so the cost is
//! slight; correctness is not negotiable.
//!
//! # Public products only
//!
//! The endpoint is a `multistore` [`Middleware`], registered after
//! `AwsBackendAuth`, so it runs with the caller's identity resolved and with the
//! gateway's *authorized* `BucketConfig` in hand — credentials federated, if the
//! connection needs them. It still serves only products that are public, and
//! that is a deliberate, written-down check rather than an emergent property of
//! the lookup: [`SourceProduct::is_public`] is tested explicitly before a single
//! byte is read, because everything that follows is written to an edge cache
//! shared across every anonymous caller. A tile-shaped key in a non-public
//! product is handed on to the ordinary object pipeline untouched, so private
//! tilesets keep working over the object path with ordinary authorization, and
//! nothing this endpoint does can confirm their existence.
//!
//! [`Middleware`]: multistore::middleware::Middleware
//! [`SourceProduct::is_public`]: crate::source_api::types::SourceProduct::is_public

// ── Path parsing ────────────────────────────────────────────────────
//
// Wasm-free so it can be unit-tested natively (see `tests/tiles.rs`), despite
// the crate's `[lib] test = false`. The handler below is wasm-only.

/// Suffix that marks the archive object within a tile URL.
const ARCHIVE_SUFFIX: &str = ".pmtiles";

/// The archive suffix followed by the separator that must precede the tile
/// coordinates. Matching on this rather than on [`ARCHIVE_SUFFIX`] alone is what
/// keeps a plain `GET` of the archive itself out of this handler.
const ARCHIVE_MARKER: &str = ".pmtiles/";

/// Default `max-age` on served tiles, and the TTL on the per-isolate directory
/// caches.
///
/// Protomaps' own deployments default to 86400. One hour is deliberately more
/// conservative: objects here are mutable (the proxy supports `PUT`), this
/// endpoint keys its cache on the archive path rather than its ETag, and a day
/// of stale tiles after a re-publish is a worse failure than a slightly colder
/// cache. Tune with `TILE_CACHE_MAX_AGE`.
pub(crate) const DEFAULT_TILE_MAX_AGE: u32 = 3600;

/// Leaf name that requests the TileJSON document rather than a tile.
const TILEJSON_LEAF: &str = "tiles.json";

/// What a tile-endpoint URL is asking for, and which archive it is asking of.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TileTarget<'a> {
    /// Object key of the archive, including the `.pmtiles` suffix, relative to
    /// the product root. Never has a leading or trailing slash.
    pub archive_key: &'a str,
    pub wanted: Wanted,
}

/// The resource requested from an archive.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum Wanted {
    Tile { z: u8, x: u32, y: u32, ext: TileExt },
    TileJson,
}

/// Tile extensions this endpoint answers to. The archive's own `tile_type`
/// decides the `Content-Type`; the extension is matched against it so a URL
/// cannot mislabel a tile (asking for `.png` from a vector archive is a 404,
/// not a PNG-labelled protobuf).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum TileExt {
    Mvt,
    Png,
    Jpeg,
    Webp,
    Avif,
}

impl TileExt {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            // `.pbf` is the older spelling of a vector tile and is what a lot
            // of existing XYZ URL templates say; accept both.
            "mvt" | "pbf" => Self::Mvt,
            "png" => Self::Png,
            "jpg" | "jpeg" => Self::Jpeg,
            "webp" => Self::Webp,
            "avif" => Self::Avif,
            _ => return None,
        })
    }

    /// The archive tile type this extension names.
    pub(crate) fn tile_type(self) -> pmtiles::TileType {
        match self {
            Self::Mvt => pmtiles::TileType::Mvt,
            Self::Png => pmtiles::TileType::Png,
            Self::Jpeg => pmtiles::TileType::Jpeg,
            Self::Webp => pmtiles::TileType::Webp,
            Self::Avif => pmtiles::TileType::Avif,
        }
    }

    /// The extension for an archive's tile type, or `None` for a type this
    /// endpoint will not serve.
    ///
    /// This is the single mapping between what an archive holds and what a URL
    /// may ask for. Comparing the two as enum values, rather than as a pair of
    /// parallel string tables, is what makes "advertise `.mvt`, then 404 every
    /// `.mvt` request" unrepresentable.
    pub(crate) fn for_tile_type(t: pmtiles::TileType) -> Option<Self> {
        Some(match t {
            pmtiles::TileType::Mvt => Self::Mvt,
            pmtiles::TileType::Png => Self::Png,
            pmtiles::TileType::Jpeg => Self::Jpeg,
            pmtiles::TileType::Webp => Self::Webp,
            pmtiles::TileType::Avif => Self::Avif,
            pmtiles::TileType::Unknown | pmtiles::TileType::Mlt => return None,
        })
    }

    /// The `Content-Type` to serve this tile with — pmtiles' own answer for the
    /// type, so the two can never drift.
    pub(crate) fn content_type(self) -> &'static str {
        self.tile_type().content_type()
    }

    /// Canonical spelling, used to build cache keys so that `.pbf` and `.mvt`
    /// (or `.jpg` and `.jpeg`) share one entry instead of caching the same
    /// bytes twice.
    pub(crate) fn canonical(self) -> &'static str {
        match self {
            Self::Mvt => "mvt",
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
            Self::Avif => "avif",
        }
    }
}

/// Parse an object key into a tile-endpoint target, or `None` if it is an
/// ordinary object read.
///
/// **This runs on every request that reaches the router**, because the route it
/// backs is a catch-all over object keys — so it must stay allocation-free and
/// do no I/O, and it must decline anything it is not certain about. Returning
/// `Some` for an ordinary key would shadow a real object.
///
/// Keys are matched on the *last* `.pmtiles/` in the path, so an archive nested
/// under a directory that itself ends in `.pmtiles` still resolves to the
/// innermost archive.
pub(crate) fn parse_target(key: &str) -> Option<TileTarget<'_>> {
    let idx = key.rfind(ARCHIVE_MARKER)?;
    let archive_key = &key[..idx + ARCHIVE_SUFFIX.len()];
    let rest = &key[idx + ARCHIVE_MARKER.len()..];

    // The archive key has to be canonical for the same reason the coordinates
    // do, and the reason is sharper here. `object_store::path::Path::from`
    // *collapses* empty segments and strips `.`/`..`, so `a//b.pmtiles` and
    // `a/./b.pmtiles` name the same stored object — while `cache_key` keeps `/`
    // as structure and would mint a separate edge-cache entry, and a separate
    // per-isolate directory-cache entry, for every spelling. Declining here is
    // what keeps one tile to one URL. The main pipeline reaches the same place
    // via `Path::parse` + `validate_key`.
    if !is_canonical_key(archive_key) {
        return None;
    }

    if rest == TILEJSON_LEAF {
        return Some(TileTarget {
            archive_key,
            wanted: Wanted::TileJson,
        });
    }

    let mut segments = rest.split('/');
    let z = segments.next()?;
    let x = segments.next()?;
    let y_ext = segments.next()?;
    // Exactly three segments: `5/9/12.mvt`. A fourth means this is a deeper
    // object path that merely looks tile-shaped, and is not ours.
    if segments.next().is_some() {
        return None;
    }

    let (y, ext) = y_ext.split_once('.')?;
    Some(TileTarget {
        archive_key,
        wanted: Wanted::Tile {
            z: parse_coord::<u8>(z)?,
            x: parse_coord::<u32>(x)?,
            y: parse_coord::<u32>(y)?,
            ext: TileExt::parse(ext)?,
        },
    })
}

/// Whether an object key is in the one spelling that names its object.
///
/// Rejects empty segments (`a//b`), and the relative segments `.` and `..`,
/// which `object_store::path::Path::from` would silently normalize away —
/// leaving several URLs that all read one object.
fn is_canonical_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('/')
        && key
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// Parse a canonical decimal coordinate.
///
/// Rejects anything `FromStr` would otherwise wave through — a leading `+`,
/// leading zeros, underscores — so one tile has exactly one URL. Without this,
/// `/5/9/12.mvt` and `/05/9/12.mvt` would be distinct cache entries holding
/// identical bytes, and an attacker could inflate the cache with unbounded
/// spellings of the same tile.
fn parse_coord<T: std::str::FromStr>(s: &str) -> Option<T> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if s.len() > 1 && s.starts_with('0') {
        return None;
    }
    s.parse().ok()
}

/// Field names `tilejson::TileJSON` serializes itself, which therefore must not
/// also appear in its flattened `other` map.
///
/// `other` carries `#[serde(flatten)]` and is emitted *after* these, so a key
/// here that also lands in `other` produces a JSON document with the field
/// twice — and `JSON.parse` keeps the last one.
const TILEJSON_RESERVED_KEYS: &[&str] = &[
    "tiles",
    "vector_layers",
    "attribution",
    "bounds",
    "center",
    "data",
    "description",
    "fillzoom",
    "grids",
    "legend",
    "maxzoom",
    "minzoom",
    "name",
    "scheme",
    "template",
    "version",
];

/// Drop archive-metadata keys that would shadow a field the TileJSON document
/// sets itself.
///
/// `AsyncPmTilesReader::parse_tilejson` copies unrecognized metadata straight
/// into `TileJSON::other` (`pmtiles-0.24.0/src/async_reader.rs:289,298`), and
/// that metadata is written by whoever published the archive. Anyone can publish
/// a public product here, so an archive carrying
/// `{"tiles": ["https://elsewhere.example/{z}/{x}/{y}.mvt"]}` would have its
/// value emitted after the proxy's own `tiles`, and every map client reading
/// that TileJSON from a source.coop URL would fetch its tiles from the
/// publisher's chosen origin instead. The proxy's own values win.
pub(crate) fn strip_reserved_tilejson_keys(
    other: &mut std::collections::BTreeMap<String, serde_json::Value>,
) {
    for key in TILEJSON_RESERVED_KEYS {
        other.remove(*key);
    }
}

/// Strong `ETag` for a tile body: the leading 128 bits of its SHA-256, quoted.
///
/// Content-derived rather than taken from the archive, so two versions of an
/// archive that carry identical bytes for a tile share an ETag and a client
/// holding either revalidates to a `304`. Tiles are tens of kilobytes and this
/// only runs on a cache miss, so the hash is not a cost that shows.
pub(crate) fn tile_etag(bytes: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    let mut etag = String::with_capacity(34);
    etag.push('"');
    for b in &digest[..16] {
        etag.push_str(&format!("{b:02x}"));
    }
    etag.push('"');
    etag
}

/// Whether an `If-None-Match` value matches `etag` under RFC 9110 §13.1.2 weak
/// comparison: a `W/` prefix on either side is ignored, `*` matches anything,
/// and the header may list several tags.
pub(crate) fn if_none_match_matches(header: &str, etag: &str) -> bool {
    fn bare(tag: &str) -> &str {
        let tag = tag.trim();
        tag.strip_prefix("W/").unwrap_or(tag)
    }
    let wanted = bare(etag);
    header
        .split(',')
        .map(bare)
        .any(|candidate| candidate == "*" || candidate == wanted)
}

/// PMTiles v3 header size, and the window pmtiles reads up front for the header
/// plus the root directory. Mirrored from `pmtiles::header`, where both are
/// crate-private, and pinned by `root_directory_is_addressable`'s tests.
const PMTILES_HEADER_SIZE: u64 = 127;
const PMTILES_MAX_INITIAL_BYTES: u64 = 16_384;

/// Whether pmtiles can parse this archive's root directory without panicking.
///
/// `AsyncPmTilesReader::try_from_cached_source` validates only the magic number
/// before doing
/// `initial_bytes.split_off(root_offset - HEADER_SIZE).split_to(root_length)`
/// (`pmtiles-0.24.0/src/async_reader.rs:97`), on a buffer of at most
/// `MAX_INITIAL_BYTES`. `root_offset` and `root_length` are read straight off
/// disk with no bounds check, so an archive carrying valid magic and
/// `root_offset = 0` underflows that subtraction to a huge index and `split_off`
/// panics — outside the `catch_unwind` that guards the header field reads.
///
/// On wasm32 a panic is effectively an abort: the request dies as a runtime
/// exception and the isolate is torn down, taking every concurrent request with
/// it. Products here are user-published and a truncated multipart upload lands
/// in the same place, so this is reachable by accident, not just by malice.
/// Refusing here turns an isolate kill into a 404.
///
/// `header` is the first bytes of the object; anything shorter than the fields
/// it needs is not a v3 archive either.
pub(crate) fn root_directory_is_addressable(header: &[u8], object_size: u64) -> bool {
    // magic(7) + version(1) + root_offset(8) + root_length(8)
    if header.len() < 24 || &header[..7] != b"PMTiles" {
        return false;
    }
    let read_u64 = |at: usize| {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&header[at..at + 8]);
        u64::from_le_bytes(buf)
    };
    let root_offset = read_u64(8);
    let root_length = read_u64(16);

    // The subtraction pmtiles is about to do must not underflow, and the slice
    // it then takes must lie inside the window it actually read.
    let window = object_size.min(PMTILES_MAX_INITIAL_BYTES);
    root_offset >= PMTILES_HEADER_SIZE
        && root_offset
            .checked_add(root_length)
            .is_some_and(|e| e <= window)
}

/// Percent-encode set for one path segment, shared by the cache key and the
/// TileJSON template. Mirrors `source_api::cache::PATH_SEGMENT`.
pub(crate) const PATH_SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Percent-encode an object key for use inside a URL, keeping `/` as structure.
///
/// The values this is applied to come off an already-decoded request path, so
/// re-encoding is what makes them safe to paste back into a URL. Without it an
/// archive key holding a `#` truncates every advertised tile URL at the
/// fragment, and one holding a space breaks the template outright.
pub(crate) fn encode_path(path: &str) -> String {
    path.split('/')
        .map(|seg| percent_encoding::utf8_percent_encode(seg, PATH_SEGMENT).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// Join `backend_prefix` to an object key the way multistore's own
/// `apply_backend_prefix` does.
///
/// The separator is normalized rather than assumed. `resolve_product` builds the
/// prefix by concatenating a connection's `base_prefix` with a mirror's
/// `prefix`, and normalizes neither, so a mirror registered without a trailing
/// slash is ordinary and every other read path copes with it. Concatenating raw
/// turned `cholmes/nyc-taxi-zones` + `taxi_zones.pmtiles` into
/// `cholmes/nyc-taxi-zonestaxi_zones.pmtiles`, so every tile 404'd for that
/// product while the identical archive streamed fine as an object.
pub(crate) fn join_backend_prefix(prefix: Option<&str>, key: &str) -> String {
    match prefix {
        Some(prefix) => {
            let prefix = prefix.trim_end_matches('/');
            if prefix.is_empty() {
                key.to_string()
            } else {
                format!("{prefix}/{key}")
            }
        }
        None => key.to_string(),
    }
}

// ── Tile middleware ─────────────────────────────────────────────────
//
// Wasm-only: reads through `object_store` and the Workers Cache API.

#[cfg(target_arch = "wasm32")]
pub(crate) use handler::PmTilesMiddleware;

#[cfg(target_arch = "wasm32")]
mod handler {
    use super::{parse_target, TileExt, Wanted, PMTILES_HEADER_SIZE};
    use crate::source_api::SourceCoopRegistry;
    use multistore::error::ProxyError;
    use multistore::middleware::{DispatchContext, Middleware, Next};
    use multistore::route_handler::{HandlerAction, ProxyResponseBody, ProxyResult};
    use multistore::types::{BucketConfig, S3Operation};
    use object_store::ObjectStore;
    use percent_encoding::utf8_percent_encode;
    use pmtiles::{AsyncPmTilesReader, HashMapCache, PmtError, TileCoord};
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    /// Synthetic origin for tile cache keys. Never resolved — the Cache API
    /// only requires a well-formed URL — but kept distinct from
    /// `data.source.coop` so a tile entry can never collide with a real object
    /// URL that some other code path might cache.
    const CACHE_ORIGIN: &str = "https://pmtiles-cache.source.coop";

    /// Header stored on a cached entry recording when it was written (ms since
    /// the epoch), so a hit can report a truthful `Age`. Without it a hit
    /// re-states the full `max-age` and a client treats a tile stored 59 minutes
    /// ago as fresh for another hour — doubling the effective staleness.
    const STORED_AT_HEADER: &str = "x-tile-stored-at";

    /// How long reader cache entries live.
    ///
    /// This is a memory bound, not a correctness one. Correctness comes from the
    /// ETag in the cache id: a re-published archive gets a different id and so
    /// misses, rather than having the previous archive's leaf directory replayed
    /// against its bytes. The TTL is what stops an isolate holding directories
    /// for archives nobody is asking about any more. Kept equal to the tile TTL
    /// so both tiers age out together, so it is derived rather than repeated.
    fn reader_ttl_ms(tile_max_age: u32) -> f64 {
        f64::from(tile_max_age) * 1000.0
    }

    /// Per-isolate PMTiles directory caches, keyed by `bucket/archive_key@etag`.
    ///
    /// What this saves is *leaf* directory reads. The header and root directory
    /// are re-read either way — `try_from_cached_source` always issues its
    /// `read(0, MAX_INITIAL_BYTES)` before the cache is consulted — but a deep
    /// archive walks a leaf directory per tile, and on a cache-cold viewport
    /// those are the reads that add up.
    ///
    /// `HashMapCache` keys its entries by byte offset alone, so each archive
    /// must get its own instance or offsets from one would be served for
    /// another. The ETag is in the id for the same reason at one remove: two
    /// versions of an archive that reuse a leaf offset — routine when the same
    /// tool re-encodes — are different archives as far as those offsets go.
    ///
    /// `std::sync::Mutex` is free here: a Workers isolate is single-threaded, so
    /// the lock is never contended. Entries carry an insertion timestamp and are
    /// treated as absent past [`reader_ttl_ms`].
    #[allow(clippy::type_complexity)]
    static DIR_CACHES: OnceLock<Mutex<HashMap<String, (f64, HashMapCache)>>> = OnceLock::new();

    /// Maximum archives whose directories one isolate will cache at a time.
    const MAX_DIR_CACHES: usize = 64;

    /// Fetch (or create) the directory cache for one archive.
    fn dir_cache_for(id: &str, tile_max_age: u32) -> HashMapCache {
        let now = js_sys::Date::now();
        let map = DIR_CACHES.get_or_init(|| Mutex::new(HashMap::new()));
        // A poisoned lock can only happen after a panic in this critical
        // section, which holds no user data and cannot fail; recover rather
        // than propagate, since a poisoned directory cache must not take the
        // tile endpoint down for the life of the isolate.
        let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());

        // Drop every expired entry, not just this one: the map is per-isolate
        // and otherwise grows without bound across archives.
        guard.retain(|_, (inserted, _)| now - *inserted < reader_ttl_ms(tile_max_age));

        // A TTL alone does not bound this. Entries are only ever inserted for an
        // archive that exists (the caller `head`s it first), but a public
        // account can hold arbitrarily many archives and nothing rate-limits an
        // anonymous crawler, so the map — and the O(n) sweep above that every
        // tile pays — would still grow with traffic. Past the cap, start over
        // rather than serve from a map that costs more to scan than it saves.
        if guard.len() >= MAX_DIR_CACHES && !guard.contains_key(id) {
            tracing::warn!(
                entries = guard.len(),
                "pmtiles directory cache hit its entry cap; clearing"
            );
            guard.clear();
        }

        if let Some((_, cache)) = guard.get(id) {
            return HashMapCache {
                cache: cache.cache.clone(),
            };
        }
        let cache = HashMapCache::default();
        let handle = HashMapCache {
            cache: cache.cache.clone(),
        };
        guard.insert(id.to_string(), (now, cache));
        handle
    }

    /// Serves PMTiles tiles and TileJSON for public products.
    ///
    /// A [`Middleware`] rather than a route handler so that it runs after
    /// identity resolution, with the gateway's authorized [`BucketConfig`] in
    /// hand — and after `AwsBackendAuth`, so that config already carries
    /// federated credentials where the connection needs them. Everything it
    /// does not claim goes to `next` untouched.
    pub(crate) struct PmTilesMiddleware {
        registry: SourceCoopRegistry,
        tile_max_age: u32,
        /// Fallback public origin for the tile URL template in TileJSON, used
        /// only when a request carries no `Host`.
        public_base_url: String,
    }

    impl PmTilesMiddleware {
        pub(crate) fn new(
            registry: SourceCoopRegistry,
            tile_max_age: u32,
            public_base_url: String,
        ) -> Self {
            Self {
                registry,
                tile_max_age,
                public_base_url,
            }
        }
    }

    impl Middleware for PmTilesMiddleware {
        async fn handle<'a>(
            &'a self,
            ctx: DispatchContext<'a>,
            next: Next<'a>,
        ) -> Result<HandlerAction, ProxyError> {
            // ── Decline fast ────────────────────────────────────────
            // This runs on every request, so the common case is "not a tile"
            // and must cost almost nothing. Only a plain object read can be a
            // tile: a versioned read names a specific object version, which a
            // tile is not.
            let operation: &'a S3Operation = ctx.operation;
            let headers: &'a http::HeaderMap = ctx.headers;
            let (bucket, key, head_only) = match operation {
                S3Operation::GetObject {
                    bucket,
                    key,
                    version: None,
                    ..
                } => (bucket.as_str(), key.as_str(), false),
                S3Operation::HeadObject { bucket, key, .. } => {
                    (bucket.as_str(), key.as_str(), true)
                }
                _ => return next.run(ctx).await,
            };
            // Bucket names reach the gateway already folded to
            // `account:product`; anything else is not a product path.
            let Some((account, product)) = bucket.split_once(crate::BUCKET_SEPARATOR) else {
                return next.run(ctx).await;
            };
            let Some(target) = parse_target(key) else {
                return next.run(ctx).await;
            };
            // The gateway resolves a config for every bucket-scoped operation
            // before any middleware runs, so this is defensive.
            let Some(config) = ctx.bucket_config.as_deref() else {
                return next.run(ctx).await;
            };

            let result = serve(
                &self.registry,
                self.tile_max_age,
                &public_base_url(headers, &self.public_base_url),
                bucket,
                account,
                product,
                config,
                &target,
                head_only,
                headers,
            )
            .await;

            match result {
                Ok(Some(response)) => Ok(HandlerAction::Response(response)),
                // Not ours after all: the product is not public, or the named
                // archive does not exist. The key may be a real object living
                // under a directory that merely ends in `.pmtiles`, and the
                // object pipeline can still serve it — with the caller's own
                // authorization, which this middleware never consults.
                Ok(None) => next.run(ctx).await,
                Err(e) => {
                    tracing::warn!(
                        bucket = %bucket,
                        archive = %target.archive_key,
                        "pmtiles tile request failed: {:?}",
                        e
                    );
                    Err(e)
                }
            }
        }
    }

    /// Serve one tile or TileJSON document.
    ///
    /// `Ok(None)` means "this key turned out not to be ours" — see the caller.
    #[allow(clippy::too_many_arguments)]
    async fn serve(
        registry: &SourceCoopRegistry,
        tile_max_age: u32,
        public_base_url: &str,
        bucket: &str,
        account: &str,
        product: &str,
        config: &BucketConfig,
        target: &super::TileTarget<'_>,
        head_only: bool,
        request_headers: &http::HeaderMap,
    ) -> Result<Option<ProxyResult>, ProxyError> {
        // ── Reject an impossible coordinate before spending anything ─
        // `TileCoord::new` rejects an x/y outside the 2^z grid. Doing it first,
        // rather than after a control-plane call and a 16 KiB origin read, is
        // what keeps `/2/4000000000/4000000000.mvt` from costing a full backend
        // round trip — and it costs nothing on the path that matters.
        if let Wanted::Tile { z, x, y, .. } = target.wanted {
            TileCoord::new(z, x, y)
                .map_err(|_| ProxyError::NoSuchKey(format!("invalid tile {z}/{x}/{y}")))?;
        }

        // ── Public products only ────────────────────────────────────
        // The gateway has already authorized *this caller's* read. This asks
        // the different question the shared cache needs answered — would an
        // anonymous caller see it, and is it actually public? An unlisted
        // product resolves anonymously without being public, hence the
        // explicit `is_public`. Anything else is handed on, not refused: the
        // object pipeline answers with the caller's own authorization, and
        // this endpoint confirms nothing about a product it does not serve.
        //
        // Ahead of the cache lookup, not behind it. The cache records nothing
        // about visibility, so checking it first meant a product made private
        // — or disabled by a takedown — kept serving tiles to anyone for the
        // rest of the TTL. One call, itself cached for `PRODUCT_CACHE_SECS`,
        // buys a revocation lag that matches every other path here.
        match registry.get_public_product(account, product).await {
            Ok(meta) if meta.is_public() => {}
            Ok(_) | Err(ProxyError::BucketNotFound(_)) | Err(ProxyError::AccessDenied) => {
                return Ok(None)
            }
            Err(e) => return Err(e),
        }

        let if_none_match = request_headers
            .get(http::header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok());

        // ── Edge cache ──────────────────────────────────────────────
        let cache_key = cache_key(bucket, target);
        let cache = worker::Cache::default();
        if let Ok(Some(mut hit)) = cache.get(&cache_key, false).await {
            // A remembered miss: the archive is real and public but has no
            // tile here. Answering from the cache is the whole point — sparse
            // archives are the norm, and without this every empty tile outside
            // the footprint re-pays the cold path for every user, forever.
            if hit.status_code() == 404 {
                return Err(ProxyError::NoSuchKey(describe_missing(target)));
            }
            let bytes = hit
                .bytes()
                .await
                .map_err(|e| ProxyError::Internal(format!("cache body read failed: {e}")))?;
            let stored = |name: &str| hit.headers().get(name).ok().flatten();
            let content_type =
                stored("content-type").unwrap_or_else(|| "application/octet-stream".to_string());
            let etag = stored("etag").unwrap_or_else(|| super::tile_etag(&bytes));
            let age = stored(STORED_AT_HEADER)
                .and_then(|s| s.parse::<f64>().ok())
                .map(|stored_at| ((js_sys::Date::now() - stored_at) / 1000.0).max(0.0) as u64);
            if if_none_match.is_some_and(|inm| super::if_none_match_matches(inm, &etag)) {
                return Ok(Some(not_modified(&etag, tile_max_age, true, age)));
            }
            return Ok(Some(tile_response(
                bytes,
                &content_type,
                tile_max_age,
                true,
                head_only,
                &etag,
                age,
            )));
        }

        // ── Open the archive ────────────────────────────────────────
        let archive_key = target.archive_key;
        let object_key = super::join_backend_prefix(config.backend_prefix.as_deref(), archive_key);
        let store = build_store(config)?;
        let path = object_store::path::Path::from(object_key);

        // One `head` before the reader. It settles two things the reader cannot:
        // whether the archive exists at all (absent → decline, so a real object
        // under a `*.pmtiles/` directory stays reachable), and its ETag, which
        // is what makes a directory cache safe to reuse across requests.
        // `get_opts` with `head: true` rather than `ObjectStoreExt::head`: the
        // latter returns an opaque future and so is not on the `dyn ObjectStore`
        // vtable this store is behind.
        let head_opts = object_store::GetOptions {
            head: true,
            ..Default::default()
        };
        let object = match store.get_opts(&path, head_opts).await {
            Ok(r) => r.meta,
            Err(object_store::Error::NotFound { .. }) => return Ok(None),
            Err(e) => return Err(map_pmt_error(PmtError::ObjectStore(e), archive_key)),
        };

        // `AsyncBackend` addresses the archive with `usize`, which is 32 bits on
        // wasm32, so pmtiles truncates any offset past 4 GiB and reads the wrong
        // bytes. Nothing downstream catches it — the truncated read is the right
        // *length*, just from the wrong place — so a planet-scale archive would
        // either fail to gunzip or hand back arbitrary bytes labelled as a tile.
        // Refuse instead of serving garbage into a shared cache. This is fixable
        // only upstream: the truncation happens before any backend is called.
        if object.size > u64::from(u32::MAX) {
            return Err(ProxyError::NoSuchKey(format!(
                "{archive_key} is larger than 4 GiB, which this endpoint cannot address"
            )));
        }

        // Validate the header ourselves before handing the archive to pmtiles,
        // which would otherwise panic on a malformed one and take the isolate
        // down. Cheap: 24 bytes, and only on a cold tile.
        if object.size < PMTILES_HEADER_SIZE {
            return Err(ProxyError::NoSuchKey(format!(
                "{archive_key} is not a readable PMTiles v3 archive"
            )));
        }
        let probe_opts = object_store::GetOptions {
            range: Some((0..24u64).into()),
            ..Default::default()
        };
        let header_bytes = match store.get_opts(&path, probe_opts).await {
            Ok(r) => r
                .bytes()
                .await
                .map_err(|e| map_pmt_error(PmtError::ObjectStore(e), archive_key))?,
            Err(object_store::Error::NotFound { .. }) => return Ok(None),
            Err(e) => return Err(map_pmt_error(PmtError::ObjectStore(e), archive_key)),
        };
        if !super::root_directory_is_addressable(&header_bytes, object.size) {
            return Err(ProxyError::NoSuchKey(format!(
                "{archive_key} is not a readable PMTiles v3 archive"
            )));
        }

        // The directory cache is keyed by the archive's ETag, not just its path.
        // `HashMapCache` indexes entries by absolute byte offset alone, so a
        // re-published archive that happens to reuse a leaf offset — routine
        // when the same tool re-encodes — would otherwise have the *previous*
        // leaf replayed against the new bytes, and `PmtError::SourceModified`
        // cannot catch it because both reads in a request see the new object.
        // Versioning the key makes a re-upload miss instead of corrupt.
        let version = object.e_tag.as_deref().unwrap_or("-");
        let reader = AsyncPmTilesReader::try_from_cached_source(
            pmtiles::ObjectStoreBackend::new(store, path),
            dir_cache_for(&format!("{bucket}/{archive_key}@{version}"), tile_max_age),
        )
        .await
        .map_err(|e| map_pmt_error(e, archive_key))?;

        let header = reader.get_header();

        // An archive whose tile type this endpoint cannot serve has no tile URL
        // to advertise either. Failing here is louder than handing the client a
        // TileJSON whose every tile request would 404 — which is what a blind
        // `.mvt` fallback did, rendering a blank map with no error anywhere.
        let Some(archive_ext) = TileExt::for_tile_type(header.tile_type) else {
            return Err(ProxyError::NoSuchKey(format!(
                "{archive_key} holds {:?} tiles, which this endpoint cannot serve",
                header.tile_type
            )));
        };

        let (bytes, content_type): (Vec<u8>, &'static str) = match target.wanted {
            Wanted::TileJson => {
                // Every interpolated segment is percent-encoded. These arrive
                // from an already-decoded request path, so an archive key
                // holding a space or a `#` would otherwise be advertised raw
                // and a client would truncate the URL at the fragment.
                let template = format!(
                    "{}/{}/{}/{}/{{z}}/{{x}}/{{y}}.{}",
                    public_base_url.trim_end_matches('/'),
                    super::encode_path(account),
                    super::encode_path(product),
                    super::encode_path(archive_key),
                    archive_ext.canonical(),
                );
                let mut tj = reader
                    .parse_tilejson(vec![template])
                    .await
                    .map_err(|e| map_pmt_error(e, archive_key))?;
                // The archive's metadata is publisher-controlled and lands in a
                // flattened map that serializes after these fields.
                super::strip_reserved_tilejson_keys(&mut tj.other);
                let body = serde_json::to_vec(&tj)
                    .map_err(|e| ProxyError::Internal(format!("tilejson encode failed: {e}")))?;
                (body, "application/json")
            }
            Wanted::Tile { z, x, y, ext } => {
                // The archive decides the tile type; the URL may not contradict
                // it, or we would hand back a protobuf labelled `image/png`.
                if archive_ext != ext {
                    return Err(ProxyError::NoSuchKey(format!(
                        "archive holds .{} tiles, not .{}",
                        archive_ext.canonical(),
                        ext.canonical()
                    )));
                }
                let coord = TileCoord::new(z, x, y)
                    .map_err(|_| ProxyError::NoSuchKey(format!("invalid tile {z}/{x}/{y}")))?;
                // Decompressed, and served without `Content-Encoding` — see the
                // module docs: relaying the stored bytes compressed gets them
                // re-encoded twice more on the way out.
                match reader
                    .get_tile_decompressed(coord)
                    .await
                    .map_err(|e| map_pmt_error(e, archive_key))?
                {
                    Some(raw) => (raw.to_vec(), ext.content_type()),
                    None => {
                        // Remember the miss. Safe to share: this is only
                        // reached for a public archive that exists, after the
                        // visibility gate above, and it ages out with the
                        // tiles themselves — a re-upload that fills the gap
                        // appears exactly as late as a changed tile would.
                        if let Err(e) =
                            put_negative_in_cache(&cache, &cache_key, tile_max_age).await
                        {
                            tracing::warn!("negative tile cache put failed: {}", e);
                        }
                        return Err(ProxyError::NoSuchKey(format!("no tile at {z}/{x}/{y}")));
                    }
                }
            }
        };

        // ── Populate the edge cache ─────────────────────────────────
        // Best-effort: a cache failure must not fail the request.
        let etag = super::tile_etag(&bytes);
        if let Err(e) = put_in_cache(
            &cache,
            &cache_key,
            &bytes,
            content_type,
            &etag,
            tile_max_age,
        )
        .await
        {
            tracing::warn!("tile cache put failed: {}", e);
        }

        if if_none_match.is_some_and(|inm| super::if_none_match_matches(inm, &etag)) {
            return Ok(Some(not_modified(&etag, tile_max_age, false, None)));
        }
        Ok(Some(tile_response(
            bytes,
            content_type,
            tile_max_age,
            false,
            head_only,
            &etag,
            None,
        )))
    }

    /// Error text for a tile that a remembered miss says is not there.
    fn describe_missing(target: &super::TileTarget<'_>) -> String {
        match target.wanted {
            Wanted::Tile { z, x, y, .. } => format!("no tile at {z}/{x}/{y}"),
            Wanted::TileJson => format!("{} has no TileJSON", target.archive_key),
        }
    }

    /// The origin to advertise in TileJSON tile templates.
    ///
    /// Taken from the request's own `Host` when there is one, and only then
    /// from configuration. The template has to point at the hostname the client
    /// actually reached, and a configured value cannot know it: `PUBLIC_BASE_URL`
    /// defaults to `OIDC_PROVIDER_ISSUER`, which preview deployments pin to
    /// `https://data.staging.source.coop` for JWKS reasons while themselves
    /// serving on `pr-N.*.workers.dev`. Every preview would otherwise hand out a
    /// tiles.json aimed at staging — and the template test, which rewrites the
    /// production host, would silently pass by fetching staging instead of the
    /// deployment under test.
    fn public_base_url(headers: &http::HeaderMap, configured: &str) -> String {
        let host = headers
            .get(http::header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|h| !h.is_empty() && !h.contains('/'));
        match host {
            // Workers are always fronted by TLS; there is no http origin to
            // advertise. `x-forwarded-proto` is client-settable, so it is not
            // consulted — trusting it would let a caller mint http templates.
            Some(host) => format!("https://{host}"),
            None => configured.to_string(),
        }
    }

    /// Map a PMTiles/object-store failure onto the right S3 error.
    ///
    /// The distinction that matters is permanent vs transient, which here tracks
    /// client-fault vs server-fault. Anything that is a fixed property of the
    /// stored bytes — absent, not a v3 archive, a codec this build does not
    /// carry, a damaged directory — is a `404`: retrying cannot change it.
    /// Everything else is a backend fault and stays a `5xx`. Getting this wrong
    /// makes a typo'd URL look like an outage, and makes every client and CDN in
    /// the chain retry a failure that will never clear.
    fn map_pmt_error(e: PmtError, archive_key: &str) -> ProxyError {
        match e {
            // The object is absent. `object_store` also reports a 404 from the
            // backend this way.
            PmtError::ObjectStore(object_store::Error::NotFound { .. }) => {
                ProxyError::NoSuchKey(format!("{archive_key} not found"))
            }
            // The object is there but is not a PMTiles v3 archive. A short or
            // truncated file is caught before this, by
            // `root_directory_is_addressable` — pmtiles itself would panic on
            // some of those rather than return an error.
            //
            // Every one of these is a permanent property of the stored bytes,
            // not a transient backend fault: an unsupported compression codec
            // (this crate builds pmtiles without `brotli` and `zstd`), a damaged
            // directory or metadata block, a gunzip failure. Reporting them as
            // 503 told every client and CDN in the chain to retry an outage that
            // will never clear, and contradicted this function's own contract.
            PmtError::InvalidMagicNumber
            | PmtError::UnsupportedPmTilesVersion
            | PmtError::InvalidHeader
            | PmtError::InvalidTileType
            | PmtError::InvalidCompression
            | PmtError::UnsupportedCompression(..)
            | PmtError::InvalidEntry
            | PmtError::InvalidMetadata
            | PmtError::InvalidMetadataUtf8Encoding(..)
            | PmtError::Reading(..)
            | PmtError::UnexpectedNumberOfBytesReturned(..) => ProxyError::NoSuchKey(format!(
                "{archive_key} is not a readable PMTiles v3 archive"
            )),
            // The archive changed underneath a cached reader. Transient by
            // construction: the directory cache entry ages out, and the client
            // should retry.
            PmtError::SourceModified => ProxyError::BackendError(format!(
                "{archive_key} was modified while being read; retry"
            )),
            other => ProxyError::BackendError(format!("could not read {archive_key}: {other}")),
        }
    }

    /// Build an `object_store` client for a resolved product backend.
    ///
    /// Retries are disabled to match `WorkerBackend::create_paginated_store`:
    /// `object_store`'s retry path sleeps via tokio, which panics on wasm.
    fn build_store(config: &BucketConfig) -> Result<Box<dyn ObjectStore>, ProxyError> {
        use multistore::backend::{create_builder, StoreBuilder};

        // This middleware is registered after `AwsBackendAuth`, which replaces
        // the `auth_type=oidc` / `oidc_*` markers with federated credentials.
        // If the markers are still here, federation did not happen, and
        // `create_builder` would silently drop them (they are not
        // `AmazonS3ConfigKey` variants) and fall through to object_store's
        // instance-metadata provider — an unreachable 169.254.169.254 fetch from
        // inside the Worker that, with retries disabled, surfaces as a 503 on
        // every tile forever. Refuse with a permanent status instead.
        if config.backend_options.get("auth_type").map(String::as_str) == Some("oidc") {
            return Err(ProxyError::NoSuchKey(
                "tile endpoint reached a federated connection without credentials".to_string(),
            ));
        }

        let no_retry = object_store::RetryConfig {
            max_retries: 0,
            ..Default::default()
        };
        let build_err =
            |e: object_store::Error| ProxyError::ConfigError(format!("store build failed: {e}"));
        Ok(match create_builder(config)? {
            StoreBuilder::S3(b) => Box::new(b.with_retry(no_retry).build().map_err(build_err)?),
            StoreBuilder::Azure(b) => Box::new(b.with_retry(no_retry).build().map_err(build_err)?),
            StoreBuilder::Gcs(b) => Box::new(b.with_retry(no_retry).build().map_err(build_err)?),
        })
    }

    /// Cache key for one tile or TileJSON document.
    ///
    /// Built from canonical coordinates, so `/05/9/12.pbf` and `/5/9/12.mvt`
    /// share the entry they should. Carries no auth material — this endpoint is
    /// public-only, so content identity is the whole key.
    fn cache_key(bucket: &str, target: &super::TileTarget<'_>) -> String {
        let bucket = utf8_percent_encode(bucket, super::PATH_SEGMENT);
        // The archive key keeps its `/` separators (they are path structure),
        // but every other reserved character is escaped.
        let archive = super::encode_path(target.archive_key);
        match target.wanted {
            Wanted::TileJson => format!("{CACHE_ORIGIN}/{bucket}/{archive}/tiles.json"),
            Wanted::Tile { z, x, y, ext } => {
                format!(
                    "{CACHE_ORIGIN}/{bucket}/{archive}/{z}/{x}/{y}.{}",
                    ext.canonical()
                )
            }
        }
    }

    /// Headers every cached entry carries so the Cache API stores it and a later
    /// hit can report its age.
    fn cache_entry_headers(max_age: u32) -> worker::Headers {
        let headers = worker::Headers::new();
        // The Cache API ignores a response with no `max-age`/`s-maxage`.
        let _ = headers.set("cache-control", &format!("public, max-age={max_age}"));
        let _ = headers.set(STORED_AT_HEADER, &format!("{:.0}", js_sys::Date::now()));
        headers
    }

    async fn put_in_cache(
        cache: &worker::Cache,
        key: &str,
        bytes: &[u8],
        content_type: &str,
        etag: &str,
        max_age: u32,
    ) -> Result<(), worker::Error> {
        let headers = cache_entry_headers(max_age);
        let _ = headers.set("content-type", content_type);
        let _ = headers.set("etag", etag);
        let resp = worker::Response::from_bytes(bytes.to_vec())?.with_headers(headers);
        cache.put(key, resp).await
    }

    /// Remember that a public archive has no tile at this coordinate.
    async fn put_negative_in_cache(
        cache: &worker::Cache,
        key: &str,
        max_age: u32,
    ) -> Result<(), worker::Error> {
        let resp = worker::Response::empty()?
            .with_status(404)
            .with_headers(cache_entry_headers(max_age));
        cache.put(key, resp).await
    }

    fn insert(headers: &mut http::HeaderMap, name: &'static str, value: &str) {
        if let Ok(v) = http::HeaderValue::from_str(value) {
            headers.insert(name, v);
        }
    }

    /// Headers shared by every client-facing tile response, 200 or 304.
    ///
    /// `x-tile-cache` is informational: a Worker's own response never carries a
    /// meaningful `cf-cache-status` (the Worker runs in front of the CDN cache),
    /// so without this there is no way to tell a warm tile from a cold one.
    fn response_headers(
        max_age: u32,
        cache_hit: bool,
        etag: &str,
        age: Option<u64>,
    ) -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        insert(
            &mut headers,
            "cache-control",
            &format!("public, max-age={max_age}"),
        );
        insert(&mut headers, "etag", etag);
        headers.insert(
            "x-tile-cache",
            if cache_hit {
                http::HeaderValue::from_static("HIT")
            } else {
                http::HeaderValue::from_static("MISS")
            },
        );
        if let Some(age) = age {
            insert(&mut headers, "age", &age.to_string());
        }
        headers
    }

    /// Assemble the client-facing response.
    fn tile_response(
        bytes: Vec<u8>,
        content_type: &str,
        max_age: u32,
        cache_hit: bool,
        head_only: bool,
        etag: &str,
        age: Option<u64>,
    ) -> ProxyResult {
        let mut headers = response_headers(max_age, cache_hit, etag, age);
        insert(&mut headers, "content-type", content_type);
        // A HEAD must report the length it would have sent, with no body.
        insert(&mut headers, "content-length", &bytes.len().to_string());
        ProxyResult {
            status: 200,
            headers,
            body: if head_only {
                ProxyResponseBody::Empty
            } else {
                ProxyResponseBody::from_bytes(bytes.into())
            },
        }
    }

    /// The `304` for a client whose `If-None-Match` still describes the tile.
    /// Carries the same validator and freshness headers a `200` would, so the
    /// client can update its stored response (RFC 9110 §15.4.5).
    fn not_modified(etag: &str, max_age: u32, cache_hit: bool, age: Option<u64>) -> ProxyResult {
        ProxyResult {
            status: 304,
            headers: response_headers(max_age, cache_hit, etag, age),
            body: ProxyResponseBody::Empty,
        }
    }
}
