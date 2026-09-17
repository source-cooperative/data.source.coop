//! Native unit tests for the wasm-free half of `tiles` — the URL parsing that
//! decides whether a request is a tile request at all. Included via `#[path]`
//! (the lib is `cdylib` with `test = false`); the handler itself is
//! `#[cfg(target_arch = "wasm32")]` and so is absent from this build.
//!
//! This parser backs a catch-all route over *every* object key, so a false
//! positive shadows a real object. Most of these tests are about declining.

#[path = "../src/tiles.rs"]
mod tiles;

use tiles::{parse_target, TileExt, Wanted};

fn tile(key: &str) -> Option<(String, Wanted)> {
    parse_target(key).map(|t| (t.archive_key.to_string(), t.wanted))
}

// ── Accepts ─────────────────────────────────────────────────────────

#[test]
fn parses_a_tile_url() {
    assert_eq!(
        tile("nyc-taxi-zones/taxi_zones.pmtiles/5/9/12.mvt"),
        Some((
            "nyc-taxi-zones/taxi_zones.pmtiles".to_string(),
            Wanted::Tile {
                z: 5,
                x: 9,
                y: 12,
                ext: TileExt::Mvt
            }
        ))
    );
}

#[test]
fn parses_a_tilejson_url() {
    assert_eq!(
        tile("a/b/c.pmtiles/tiles.json"),
        Some(("a/b/c.pmtiles".to_string(), Wanted::TileJson))
    );
}

#[test]
fn accepts_an_archive_at_the_product_root() {
    assert_eq!(
        tile("x.pmtiles/0/0/0.mvt"),
        Some((
            "x.pmtiles".to_string(),
            Wanted::Tile {
                z: 0,
                x: 0,
                y: 0,
                ext: TileExt::Mvt
            }
        ))
    );
}

/// `.pbf` is what a lot of existing XYZ templates say, and `.jpg`/`.jpeg` both
/// occur in the wild. Both must resolve, and both must canonicalize so they
/// share one cache entry rather than storing the same bytes twice.
#[test]
fn extension_aliases_resolve_and_canonicalize() {
    for (ext, expected) in [
        ("mvt", TileExt::Mvt),
        ("pbf", TileExt::Mvt),
        ("png", TileExt::Png),
        ("jpg", TileExt::Jpeg),
        ("jpeg", TileExt::Jpeg),
        ("webp", TileExt::Webp),
        ("avif", TileExt::Avif),
    ] {
        let Some((_, Wanted::Tile { ext: got, .. })) = tile(&format!("a.pmtiles/1/0/0.{ext}"))
        else {
            panic!(".{ext} should parse as a tile");
        };
        assert_eq!(got, expected, ".{ext}");
    }
    assert_eq!(TileExt::Mvt.canonical(), "mvt");
    assert_eq!(TileExt::Jpeg.canonical(), "jpeg");
}

/// The deepest archive wins, so a directory that happens to end in `.pmtiles`
/// does not swallow an archive nested beneath it.
#[test]
fn the_innermost_archive_wins() {
    assert_eq!(
        tile("outer.pmtiles/inner.pmtiles/3/1/2.mvt"),
        Some((
            "outer.pmtiles/inner.pmtiles".to_string(),
            Wanted::Tile {
                z: 3,
                x: 1,
                y: 2,
                ext: TileExt::Mvt
            }
        ))
    );
}

#[test]
fn large_coordinates_parse() {
    // z22 is within the PMTiles spec's range; x/y there exceed u16.
    assert_eq!(
        tile("a.pmtiles/22/2097151/2097151.mvt"),
        Some((
            "a.pmtiles".to_string(),
            Wanted::Tile {
                z: 22,
                x: 2_097_151,
                y: 2_097_151,
                ext: TileExt::Mvt
            }
        ))
    );
}

// ── Declines ────────────────────────────────────────────────────────

/// The important cases: an ordinary object read must never be captured, or the
/// catch-all route would shadow real data.
#[test]
fn ordinary_object_keys_are_declined() {
    for key in [
        "countries.parquet",
        "a/b/c.tif",
        "README.md",
        "data/2024/points.fgb",
        // The archive itself: a plain GET must keep streaming the object.
        "taxi_zones.pmtiles",
        // Something adjacent to an archive, but not tile-shaped.
        "taxi_zones.pmtiles/metadata.json",
        "taxi_zones.pmtiles/",
        // A directory named like an archive with a deeper object under it.
        "taxi_zones.pmtiles/5/9/12/extra.mvt",
        // Right shape, unknown extension.
        "a.pmtiles/5/9/12.txt",
        "a.pmtiles/5/9/12",
        // `.pmtiles` not at a path boundary.
        "notpmtiles/5/9/12.mvt",
        "a.pmtilesx/5/9/12.mvt",
        "",
    ] {
        assert_eq!(tile(key), None, "should decline {key:?}");
    }
}

/// Non-canonical coordinate spellings are refused so one tile has exactly one
/// URL — otherwise `/05/9/12.mvt` and `/5/9/12.mvt` are separate cache entries
/// for identical bytes, and the cache can be inflated with spellings.
#[test]
fn non_canonical_coordinates_are_declined() {
    for key in [
        "a.pmtiles/05/9/12.mvt",
        "a.pmtiles/5/09/12.mvt",
        "a.pmtiles/5/9/012.mvt",
        "a.pmtiles/+5/9/12.mvt",
        "a.pmtiles/-5/9/12.mvt",
        "a.pmtiles/5/9/-12.mvt",
        "a.pmtiles/ 5/9/12.mvt",
        "a.pmtiles/5 /9/12.mvt",
        "a.pmtiles/5/9/1_2.mvt",
        "a.pmtiles//9/12.mvt",
        "a.pmtiles/5//12.mvt",
        "a.pmtiles/5/9/.mvt",
    ] {
        assert_eq!(tile(key), None, "should decline {key:?}");
    }
}

/// `z` is a `u8` and `x`/`y` are `u32`; anything wider must be refused rather
/// than silently wrapping into a valid-looking tile.
#[test]
fn out_of_range_coordinates_are_declined() {
    assert_eq!(tile("a.pmtiles/256/0/0.mvt"), None, "z > u8::MAX");
    assert_eq!(tile("a.pmtiles/5/4294967296/0.mvt"), None, "x > u32::MAX");
    assert_eq!(tile("a.pmtiles/5/0/4294967296.mvt"), None, "y > u32::MAX");
}

/// `0` is canonical and must still be accepted — the leading-zero rule applies
/// only to multi-digit numbers.
#[test]
fn zero_is_accepted() {
    assert!(tile("a.pmtiles/0/0/0.mvt").is_some());
}

// ── Content types ───────────────────────────────────────────────────

#[test]
fn content_types_match_the_extension() {
    assert_eq!(
        TileExt::Mvt.content_type(),
        "application/vnd.mapbox-vector-tile"
    );
    assert_eq!(TileExt::Png.content_type(), "image/png");
    assert_eq!(TileExt::Jpeg.content_type(), "image/jpeg");
    assert_eq!(TileExt::Webp.content_type(), "image/webp");
    assert_eq!(TileExt::Avif.content_type(), "image/avif");
}

/// Pinned deliberately: this is both the tile `max-age` and the TTL that bounds
/// how long a cached reader may replay byte offsets against an archive that may
/// have been re-uploaded underneath it. Changing it is a correctness decision,
/// not a tuning one.
#[test]
fn default_tile_max_age_is_one_hour() {
    assert_eq!(tiles::DEFAULT_TILE_MAX_AGE, 3600);
}
