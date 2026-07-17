//! Native unit tests for the chunk cache's pure range / key math.
//! The wasm-only backend wrapper in the same source file is cfg-gated out.

#[path = "../src/chunk_cache.rs"]
mod chunk_cache;

use chunk_cache::*;

const MIB: u64 = 1024 * 1024;

// ── parse_range ─────────────────────────────────────────────────────

#[test]
fn parse_bounded_open_and_suffix_ranges() {
    assert_eq!(
        parse_range("bytes=0-1023"),
        Some(RangeSpec::Bounded(0, 1023))
    );
    assert_eq!(parse_range("bytes=100-"), Some(RangeSpec::From(100)));
    assert_eq!(parse_range("bytes=-4096"), Some(RangeSpec::Suffix(4096)));
}

#[test]
fn parse_rejects_ineligible_ranges() {
    assert_eq!(parse_range("bytes=0-1,5-9"), None); // multi-range
    assert_eq!(parse_range("items=0-1"), None); // non-bytes unit
    assert_eq!(parse_range("bytes=-0"), None); // empty suffix
    assert_eq!(parse_range("bytes=5-2"), None); // inverted
    assert_eq!(parse_range("bytes=-"), None);
    assert_eq!(parse_range("bytes=abc-def"), None);
    assert_eq!(parse_range(""), None);
}

// ── resolve_range ───────────────────────────────────────────────────

#[test]
fn resolve_full_object_when_no_range() {
    assert_eq!(resolve_range(None, 100), Some((0, 99)));
}

#[test]
fn resolve_bounded_clamps_to_eof() {
    assert_eq!(
        resolve_range(Some(&RangeSpec::Bounded(50, 1_000_000)), 100),
        Some((50, 99))
    );
}

#[test]
fn resolve_start_beyond_eof_is_unsatisfiable() {
    assert_eq!(
        resolve_range(Some(&RangeSpec::Bounded(100, 200)), 100),
        None
    );
    assert_eq!(resolve_range(Some(&RangeSpec::From(100)), 100), None);
}

#[test]
fn resolve_open_ended_spans_to_eof() {
    assert_eq!(
        resolve_range(Some(&RangeSpec::From(10)), 100),
        Some((10, 99))
    );
}

#[test]
fn resolve_suffix_ranges() {
    assert_eq!(
        resolve_range(Some(&RangeSpec::Suffix(10)), 100),
        Some((90, 99))
    );
    // Suffix larger than the object → whole object (RFC 7233 §2.1).
    assert_eq!(
        resolve_range(Some(&RangeSpec::Suffix(500)), 100),
        Some((0, 99))
    );
}

// ── chunk math ──────────────────────────────────────────────────────

#[test]
fn aligned_range_maps_to_exact_chunk() {
    // [0, 4MiB) sits entirely in chunk 0.
    assert_eq!(chunk_index_range(0, 4 * MIB - 1, 4 * MIB), (0, 0));
}

#[test]
fn unaligned_range_spans_two_chunks() {
    assert_eq!(chunk_index_range(100, 4 * MIB + 99, 4 * MIB), (0, 1));
}

#[test]
fn single_byte_range() {
    assert_eq!(chunk_index_range(4 * MIB, 4 * MIB, 4 * MIB), (1, 1));
    assert_eq!(chunk_bounds(1, 4 * MIB, 100 * MIB), (4 * MIB, 8 * MIB - 1));
}

#[test]
fn last_chunk_trims_to_eof() {
    let len = 4 * MIB + 1000;
    assert_eq!(chunk_bounds(1, 4 * MIB, len), (4 * MIB, len - 1));
}

#[test]
fn assembly_offsets_cover_span_exactly() {
    // Simulate the assembly loop's trim math for an unaligned span.
    let (chunk_size, len) = (4 * MIB, 100 * MIB);
    let (start, end) = (chunk_size - 10, chunk_size + 9); // straddles chunks 0/1
    let (first, last) = chunk_index_range(start, end, chunk_size);
    let mut assembled = 0u64;
    for index in first..=last {
        let (cb_start, cb_end) = chunk_bounds(index, chunk_size, len);
        let from = start.max(cb_start) - cb_start;
        let to = end.min(cb_end) - cb_start + 1;
        assembled += to - from;
    }
    assert_eq!(assembled, end - start + 1);
}

// ── cache keys ──────────────────────────────────────────────────────

#[test]
fn chunk_key_varies_by_etag_chunk_size_and_index() {
    let prefix = object_cache_prefix("data.source.coop", "/acct/prod/a.parquet");
    let base = chunk_key(&prefix, "\"abc\"", 4 * MIB, 0);
    assert_ne!(base, chunk_key(&prefix, "\"xyz\"", 4 * MIB, 0));
    assert_ne!(base, chunk_key(&prefix, "\"abc\"", 8 * MIB, 0));
    assert_ne!(base, chunk_key(&prefix, "\"abc\"", 4 * MIB, 1));
    assert_ne!(chunk_key(&prefix, "\"abc\"", 4 * MIB, 0), meta_key(&prefix));
}

#[test]
fn prefix_encodes_specials_and_preserves_structure() {
    let p = object_cache_prefix("h", "/acct/prod/dir/file with space?.tif");
    assert_eq!(
        p,
        "https://h/.chunk-cache/v1/acct/prod/dir/file%20with%20space%3F.tif"
    );
    // Trailing slash stays distinct — "dir/" and "dir" are different S3 keys.
    assert_ne!(
        object_cache_prefix("h", "/a/p/dir/"),
        object_cache_prefix("h", "/a/p/dir")
    );
}

// ── backend response parsing ────────────────────────────────────────

#[test]
fn content_range_total_parses() {
    assert_eq!(parse_content_range_total("bytes 0-0/12345"), Some(12345));
    assert_eq!(parse_content_range_total("bytes 0-0/*"), None);
    assert_eq!(parse_content_range_total("bytes */100"), Some(100));
    assert_eq!(parse_content_range_total("garbage"), None);
}

#[test]
fn only_strong_etags_qualify() {
    assert!(is_strong_etag("\"abc123\""));
    assert!(is_strong_etag("\"multipart-3\""));
    assert!(!is_strong_etag("W/\"abc123\""));
    assert!(!is_strong_etag("abc123"));
    assert!(!is_strong_etag(""));
    assert!(!is_strong_etag("\""));
}

#[test]
fn object_meta_roundtrips_json() {
    let meta = ObjectMeta {
        etag: "\"abc\"".into(),
        len: 42,
        content_type: Some("image/tiff".into()),
        last_modified: None,
    };
    let back: ObjectMeta = serde_json::from_str(&serde_json::to_string(&meta).unwrap()).unwrap();
    assert_eq!(back.etag, meta.etag);
    assert_eq!(back.len, meta.len);
    assert_eq!(back.content_type, meta.content_type);
    assert_eq!(back.last_modified, None);
}

// ── constants sanity ────────────────────────────────────────────────

#[test]
fn bypass_threshold_is_chunk_aligned() {
    // A max-size span must decompose into whole chunks with no remainder,
    // and the meta TTL must actually expire entries.
    assert_eq!(MAX_CACHEABLE_SPAN % CHUNK_SIZE, 0);
    assert!(MAX_CACHEABLE_SPAN / CHUNK_SIZE <= 16); // subrequest headroom
    assert!(META_TTL_SECS > 0);
}
