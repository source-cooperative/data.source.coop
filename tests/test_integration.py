"""Integration tests for the Source Cooperative Data Proxy.

Requires the worker to be running at the URL specified by PROXY_URL
(defaults to http://localhost:8787).
"""

import os

import requests

PROXY_URL = os.environ.get("PROXY_URL", "http://localhost:8787")

# Known public product for testing
ACCOUNT = "cholmes"
PRODUCT = "admin-boundaries"
OBJECT_KEY = "countries.parquet"


def test_index():
    resp = requests.get(f"{PROXY_URL}/")
    assert resp.status_code == 200
    assert "Source Cooperative Data Proxy" in resp.text


def test_anonymous_write_denied():
    # Writes route through the gateway and are authorized there. An
    # unauthenticated write to a real product is denied at the gate (403),
    # before any backend call.
    resp = requests.put(f"{PROXY_URL}/{ACCOUNT}/{PRODUCT}/{OBJECT_KEY}")
    assert resp.status_code == 403


def test_options_cors():
    resp = requests.options(f"{PROXY_URL}/", headers={"Origin": "https://example.com"})
    assert resp.status_code == 204
    assert resp.headers["access-control-allow-origin"] == "*"
    assert "GET" in resp.headers["access-control-allow-methods"]
    assert "HEAD" in resp.headers["access-control-allow-methods"]


def test_product_listing():
    resp = requests.get(f"{PROXY_URL}/{ACCOUNT}?list-type=2&delimiter=/")
    assert resp.status_code == 200
    assert "CommonPrefixes" in resp.text
    assert f"{PRODUCT}/" in resp.text


def test_file_listing():
    resp = requests.get(
        f"{PROXY_URL}/{ACCOUNT}?list-type=2&prefix={PRODUCT}/&max-keys=5"
    )
    assert resp.status_code == 200
    assert OBJECT_KEY in resp.text


def test_head_object():
    resp = requests.head(f"{PROXY_URL}/{ACCOUNT}/{PRODUCT}/{OBJECT_KEY}")
    assert resp.status_code == 200
    assert "content-length" in resp.headers
    assert int(resp.headers["content-length"]) > 0
    assert "etag" in resp.headers
    assert "last-modified" in resp.headers
    assert resp.headers.get("accept-ranges") == "bytes"


def test_get_object_range():
    resp = requests.get(
        f"{PROXY_URL}/{ACCOUNT}/{PRODUCT}/{OBJECT_KEY}",
        headers={"Range": "bytes=0-1023"},
    )
    assert resp.status_code in (200, 206)
    assert len(resp.content) == 1024


def test_head_object_range():
    resp = requests.head(
        f"{PROXY_URL}/{ACCOUNT}/{PRODUCT}/{OBJECT_KEY}",
        headers={"Range": "bytes=0-1023"},
    )
    assert resp.status_code in (200, 206)
    content_length = int(resp.headers["content-length"])
    assert content_length == 1024


def test_get_object_range_middle():
    """Request a range in the middle of the file."""
    resp = requests.get(
        f"{PROXY_URL}/{ACCOUNT}/{PRODUCT}/{OBJECT_KEY}",
        headers={"Range": "bytes=1000-1999"},
    )
    assert resp.status_code in (200, 206)
    assert len(resp.content) == 1000


def test_cors_on_get():
    resp = requests.get(f"{PROXY_URL}/")
    assert resp.headers["access-control-allow-origin"] == "*"
    assert resp.headers["access-control-expose-headers"] == "*"


def test_not_found():
    resp = requests.get(
        f"{PROXY_URL}/{ACCOUNT}/{PRODUCT}/nonexistent-file-abc123.txt"
    )
    assert resp.status_code == 404


# --- Regression tests for bugs fixed during multistore refactor ---


def test_trailing_slash_equivalence():
    """Trailing slash on account should not break list requests."""
    resp = requests.get(
        f"{PROXY_URL}/{ACCOUNT}/?list-type=2&prefix={PRODUCT}/&max-keys=5"
    )
    assert resp.status_code == 200
    assert "<?xml" in resp.text
    assert "ListBucketResult" in resp.text


def test_url_encoded_prefix():
    """URL-encoded %2F in prefix should produce same results as literal /."""
    encoded = requests.get(
        f"{PROXY_URL}/{ACCOUNT}?list-type=2&prefix={PRODUCT}%2F&max-keys=5"
    )
    unencoded = requests.get(
        f"{PROXY_URL}/{ACCOUNT}?list-type=2&prefix={PRODUCT}/&max-keys=5"
    )
    assert encoded.status_code == 200
    assert unencoded.status_code == 200
    # Both should return the same keys
    assert encoded.text == unencoded.text


def test_xml_name_rewriting():
    """<Name> in list response should be the account, not account--product."""
    resp = requests.get(
        f"{PROXY_URL}/{ACCOUNT}?list-type=2&prefix={PRODUCT}/&max-keys=5"
    )
    assert resp.status_code == 200
    assert f"<Name>{ACCOUNT}</Name>" in resp.text
    assert "--" not in resp.text.split("<Name>")[1].split("</Name>")[0]


def test_xml_prefix_rewriting():
    """<Prefix> should contain the original prefix, not the stripped one."""
    resp = requests.get(
        f"{PROXY_URL}/{ACCOUNT}?list-type=2&prefix={PRODUCT}/&max-keys=5"
    )
    assert resp.status_code == 200
    assert f"<Prefix>{PRODUCT}/</Prefix>" in resp.text


def test_key_prefixing():
    """<Key> values in list responses should start with {product}/."""
    resp = requests.get(
        f"{PROXY_URL}/{ACCOUNT}?list-type=2&prefix={PRODUCT}/&max-keys=5"
    )
    assert resp.status_code == 200
    # Extract all <Key> values
    import re

    keys = re.findall(r"<Key>(.*?)</Key>", resp.text)
    assert len(keys) > 0, "Expected at least one key in response"
    for key in keys:
        assert key.startswith(
            f"{PRODUCT}/"
        ), f"Key '{key}' should start with '{PRODUCT}/'"


def test_object_access_via_path():
    """HEAD /{account}/{product}/{key} should return 200."""
    resp = requests.head(f"{PROXY_URL}/{ACCOUNT}/{PRODUCT}/{OBJECT_KEY}")
    assert resp.status_code == 200


# --- Chunk-aligned edge cache (issue #188) ---
# Only meaningful when the worker runs with CHUNK_CACHE_ENABLED=true (CI does);
# with it off, x-cache is absent and these assertions still hold vacuously
# where written to tolerate that.


def test_chunk_cache_roundtrip():
    """Same range twice: byte-identical bodies, valid 206 framing, and an
    x-cache disposition when the chunk cache is enabled."""
    url = f"{PROXY_URL}/{ACCOUNT}/{PRODUCT}/{OBJECT_KEY}"
    headers = {"Range": "bytes=100-1123"}
    first = requests.get(url, headers=headers)
    second = requests.get(url, headers=headers)
    assert first.status_code == 206
    assert second.status_code == 206
    assert len(first.content) == 1024
    assert first.content == second.content
    for resp in (first, second):
        assert resp.headers.get("content-range", "").startswith("bytes 100-1123/")
        assert "etag" in resp.headers
        x_cache = resp.headers.get("x-cache")
        if x_cache is not None:
            # Local cache semantics may not produce a HIT, but the header must
            # always be a known disposition.
            assert x_cache in ("HIT", "MISS", "BYPASS")


def test_chunk_cache_matches_direct_bytes():
    """A chunk-assembled range must equal the same slice of the full object."""
    url = f"{PROXY_URL}/{ACCOUNT}/{PRODUCT}/{OBJECT_KEY}"
    full = requests.get(url, headers={"Range": "bytes=0-4095"})
    slice_ = requests.get(url, headers={"Range": "bytes=1000-1999"})
    assert full.status_code == 206 and slice_.status_code == 206
    assert slice_.content == full.content[1000:2000]


def test_chunk_cache_suffix_range():
    """Suffix ranges (the parquet-footer pattern) resolve via cached metadata."""
    url = f"{PROXY_URL}/{ACCOUNT}/{PRODUCT}/{OBJECT_KEY}"
    resp = requests.get(url, headers={"Range": "bytes=-1024"})
    assert resp.status_code == 206
    assert len(resp.content) == 1024
    total = int(resp.headers["content-range"].rsplit("/", 1)[1])
    assert resp.headers["content-range"] == f"bytes {total - 1024}-{total - 1}/{total}"


def test_chunk_cache_range_beyond_eof_is_416():
    url = f"{PROXY_URL}/{ACCOUNT}/{PRODUCT}/{OBJECT_KEY}"
    head = requests.head(url)
    length = int(head.headers["content-length"])
    resp = requests.get(url, headers={"Range": f"bytes={length + 10}-{length + 20}"})
    assert resp.status_code == 416


def test_chunk_cache_conditional_request_bypasses():
    """Conditional requests keep exact origin semantics (direct path)."""
    url = f"{PROXY_URL}/{ACCOUNT}/{PRODUCT}/{OBJECT_KEY}"
    etag = requests.head(url).headers["etag"]
    resp = requests.get(
        url, headers={"Range": "bytes=0-99", "If-None-Match": etag}
    )
    assert resp.status_code == 304
    x_cache = resp.headers.get("x-cache")
    if x_cache is not None:
        assert x_cache == "BYPASS"
