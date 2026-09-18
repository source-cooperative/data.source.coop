"""Integration tests for the PMTiles Z/X/Y tile endpoint.

These run against a real PMTiles v3 archive in the public bucket
(cholmes/nyc-taxi-zones/taxi_zones.pmtiles -- MVT, z0-13, gzip-compressed
tiles), resolved through tests/stub_api.py. Nothing here is mocked below the
control plane: the worker reads real byte ranges out of real S3.
"""

import json
import os
from urllib.parse import urlparse

import requests

PROXY_URL = os.environ.get("PROXY_URL", "http://localhost:8787")

ACCOUNT = "cholmes"
PRODUCT = "nyc-taxi-zones"
ARCHIVE = "taxi_zones.pmtiles"
BASE = f"{PROXY_URL}/{ACCOUNT}/{PRODUCT}/{ARCHIVE}"

# Present in the archive: it covers NYC, and z0/0/0 exists because the archive
# starts at zoom 0.
A_TILE = "0/0/0.mvt"


def test_tile_returns_a_vector_tile():
    resp = requests.get(f"{BASE}/{A_TILE}")
    assert resp.status_code == 200, resp.text[:500]
    assert resp.headers["content-type"] == "application/vnd.mapbox-vector-tile"
    assert len(resp.content) > 0


def test_tile_body_is_a_real_mvt():
    """Guard against serving plausible-looking garbage.

    Field 3 (layers) of a Mapbox Vector Tile is encoded with tag byte 0x1a,
    which is what a valid tile starts with.
    """
    resp = requests.get(f"{BASE}/{A_TILE}")
    assert resp.status_code == 200
    assert resp.content[0] == 0x1A, f"not an MVT: {resp.content[:16].hex()}"


def test_tile_is_not_double_encoded():
    """Regression test for the encoding trap.

    Archives store tiles gzipped, and relaying those bytes with an explicit
    `Content-Encoding: gzip` reads like an optimisation. It is not: the runtime
    adds its own transfer compression on top, and the Cache API adds another
    across a put/get, so a client that decodes one layer is left holding gzip
    bytes labelled `application/vnd.mapbox-vector-tile`. Tiles are therefore
    decompressed in the worker and served plain.

    Ask for the body with no transfer coding and assert it is the protobuf
    itself, not a gzip stream.
    """
    resp = requests.get(
        f"{BASE}/{A_TILE}",
        headers={"Accept-Encoding": "identity"},
        stream=True,
    )
    assert resp.status_code == 200
    raw = resp.raw.read()
    assert raw[:2] != b"\x1f\x8b", "body is still gzip-wrapped"
    assert raw[0] == 0x1A, f"not a bare MVT: {raw[:16].hex()}"


def test_tile_is_cached_at_the_edge():
    """Second read of the same tile must come from the Cache API."""
    url = f"{BASE}/2/1/1.mvt"
    first = requests.get(url)
    assert first.status_code == 200
    second = requests.get(url)
    assert second.status_code == 200
    assert second.headers.get("x-tile-cache") == "HIT"
    assert second.content == first.content


def test_tile_sends_cache_control():
    resp = requests.get(f"{BASE}/{A_TILE}")
    assert resp.status_code == 200
    assert "max-age=" in resp.headers.get("cache-control", "")


def test_extension_aliases_share_a_cache_entry():
    """.pbf and .mvt are the same tile; they must not be cached twice."""
    requests.get(f"{BASE}/3/2/3.mvt")
    aliased = requests.get(f"{BASE}/3/2/3.pbf")
    assert aliased.status_code == 200
    assert aliased.headers.get("x-tile-cache") == "HIT"


def test_head_returns_headers_without_a_body():
    resp = requests.head(f"{BASE}/{A_TILE}")
    assert resp.status_code == 200
    assert resp.headers["content-type"] == "application/vnd.mapbox-vector-tile"
    assert int(resp.headers["content-length"]) > 0
    assert not resp.content


def test_tilejson():
    resp = requests.get(f"{BASE}/tiles.json")
    assert resp.status_code == 200, resp.text[:500]
    tj = json.loads(resp.text)
    assert tj["tilejson"].startswith("3.")
    assert len(tj["tiles"]) == 1
    template = tj["tiles"][0]
    assert template.endswith("/{z}/{x}/{y}.mvt")
    assert f"{ACCOUNT}/{PRODUCT}/{ARCHIVE}" in template
    assert tj["maxzoom"] == 13


def test_tilejson_template_names_the_host_the_client_reached():
    """The template must point at the deployment that answered.

    Configuration cannot know which one that is: PUBLIC_BASE_URL defaults to
    OIDC_PROVIDER_ISSUER, which previews pin to the staging host for JWKS
    reasons while themselves serving on pr-N.*.workers.dev. A preview that
    advertised staging would send every map client to the wrong deployment.
    """
    tj = json.loads(requests.get(f"{BASE}/tiles.json").text)
    advertised = urlparse(tj["tiles"][0]).netloc
    assert advertised == urlparse(PROXY_URL).netloc, (
        f"tiles.json advertises {advertised!r}, not the host we asked "
        f"({urlparse(PROXY_URL).netloc!r})"
    )


def test_tilejson_template_actually_resolves():
    """A TileJSON whose template 404s is worse than no TileJSON."""
    tj = json.loads(requests.get(f"{BASE}/tiles.json").text)
    template = tj["tiles"][0].replace("{z}", "0").replace("{x}", "0").replace("{y}", "0")
    # Rewrite scheme+host generically rather than substituting one hardcoded
    # origin. A hardcoded rewrite silently no-ops when the template names some
    # other host, and the test then passes by fetching a different deployment
    # instead of the one under test.
    url = PROXY_URL.rstrip("/") + urlparse(template).path
    resp = requests.get(url)
    assert resp.status_code == 200, f"{url} -> {resp.status_code}"
    assert resp.headers["content-type"] == "application/vnd.mapbox-vector-tile"


# ── Declining / errors ──────────────────────────────────────────────


def test_plain_archive_get_still_streams_the_object():
    """The catch-all route must not shadow a normal read of the archive."""
    resp = requests.get(BASE, headers={"Range": "bytes=0-6"})
    assert resp.status_code == 206
    assert resp.content == b"PMTiles"


def test_ordinary_object_read_is_unaffected():
    resp = requests.get(
        f"{PROXY_URL}/cholmes/admin-boundaries/countries.parquet",
        headers={"Range": "bytes=0-3"},
    )
    assert resp.status_code == 206
    assert resp.content == b"PAR1"


def test_listing_still_works_under_the_catch_all():
    resp = requests.get(f"{PROXY_URL}/{ACCOUNT}?list-type=2&prefix={PRODUCT}/")
    assert resp.status_code == 200
    assert ARCHIVE in resp.text


def test_missing_tile_is_404():
    # Beyond the archive's maxzoom of 13.
    resp = requests.get(f"{BASE}/14/8000/8000.mvt")
    assert resp.status_code == 404


def test_wrong_extension_for_the_archive_type_is_404():
    """A vector archive must not answer a .png request with a labelled protobuf."""
    resp = requests.get(f"{BASE}/{A_TILE.replace('.mvt', '.png')}")
    assert resp.status_code == 404


def test_non_pmtiles_object_is_404_not_a_500():
    resp = requests.get(
        f"{PROXY_URL}/cholmes/admin-boundaries/countries.parquet.pmtiles/0/0/0.mvt"
    )
    assert resp.status_code == 404


def test_non_canonical_coordinates_fall_through_to_object_read():
    """`/05/0/0.mvt` is not a tile URL, so it must be treated as an object key
    -- which does not exist, hence 404 from the normal pipeline, never a tile."""
    resp = requests.get(f"{BASE}/05/0/0.mvt")
    assert resp.status_code == 404
    assert "x-tile-cache" not in resp.headers


def test_write_to_a_tile_path_is_not_served_by_the_handler():
    resp = requests.put(f"{BASE}/{A_TILE}")
    assert resp.status_code in (403, 405)
    assert "x-tile-cache" not in resp.headers


# ── The public-only gate ────────────────────────────────────────────


def test_unlisted_product_tiles_are_refused():
    """The security property: a non-public product must never reach the shared
    edge cache, even though the control plane returns its metadata."""
    url = f"{PROXY_URL}/{ACCOUNT}/tiles-unlisted-probe/{ARCHIVE}/{A_TILE}"
    resp = requests.get(url)
    assert resp.status_code == 404, f"expected 404, got {resp.status_code}"
    assert "x-tile-cache" not in resp.headers


# ── Review fixes ────────────────────────────────────────────────────


def test_non_canonical_archive_key_falls_through_to_the_object_pipeline():
    """`Path::from` collapses empty segments, so `a//b.pmtiles` reads the same
    object as `a/b.pmtiles` — but the cache key keeps `/` as structure. Serving
    both would mint a distinct edge-cache entry per spelling of one tile."""
    resp = requests.get(f"{PROXY_URL}/{ACCOUNT}/{PRODUCT}//{ARCHIVE}/{A_TILE}")
    assert "x-tile-cache" not in resp.headers


def test_a_missing_archive_does_not_shadow_a_real_object():
    """A tile-shaped key under a directory that merely ends in `.pmtiles` is an
    ordinary object read. The handler must decline, not answer 404, or that
    object becomes permanently unreachable."""
    resp = requests.get(
        f"{PROXY_URL}/{ACCOUNT}/{PRODUCT}/definitely-absent.pmtiles/0/0/0.mvt"
    )
    # The object pipeline answers (404 here, since the fixture has no such
    # object) -- but it answers, rather than the tile handler claiming the key.
    assert "x-tile-cache" not in resp.headers
