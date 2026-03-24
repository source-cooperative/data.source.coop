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


def test_write_rejected():
    resp = requests.put(f"{PROXY_URL}/test/test/file.txt")
    assert resp.status_code == 405


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
