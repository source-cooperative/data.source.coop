"""Control-plane failure modes and subject-scoped authz, via stub probes.

The original CI incident was an untested control-plane failure path (the live
API drifted and every read 502'd). These tests keep that class covered: the
stub serves deliberately broken synthetic products (see stub_api.py), and the
proxy must map each failure to a bounded, parseable S3 XML error — never a
hang, never a success.
"""

import requests

from stub_api import ERR_500_PRODUCT, ERR_BAD_JSON_PRODUCT, RESTRICTED_PRODUCT, WRITE_ACCOUNT
from test_writes import PROXY_URL, needs_token, s3_client


def test_api_500_maps_to_s3_internal_error():
    resp = requests.get(f"{PROXY_URL}/{WRITE_ACCOUNT}/{ERR_500_PRODUCT}/any.txt")
    assert resp.status_code == 500
    assert "<Error>" in resp.text and "<Code>InternalError</Code>" in resp.text


def test_api_malformed_json_maps_to_s3_internal_error():
    resp = requests.get(f"{PROXY_URL}/{WRITE_ACCOUNT}/{ERR_BAD_JSON_PRODUCT}/any.txt")
    assert resp.status_code == 500
    assert "<Error>" in resp.text and "<Code>InternalError</Code>" in resp.text


def test_restricted_product_hidden_from_anonymous():
    """The subject-scoped product fetch returns 404 for an anonymous caller;
    the proxy must deny without ever touching the backend (the API's
    don't-leak-existence denial surfaces as NoSuchBucket)."""
    resp = requests.get(
        f"{PROXY_URL}/{WRITE_ACCOUNT}/{RESTRICTED_PRODUCT}/countries.parquet"
    )
    assert resp.status_code == 404
    assert "<Code>NoSuchBucket</Code>" in resp.text


@needs_token
def test_restricted_product_served_to_authenticated_subject():
    """The same product resolves for an authenticated subject (the proxy sends
    an Authorization header on its control-plane fetch), completing the
    visibility x identity matrix the anonymous test starts."""
    obj = s3_client().get_object(
        Bucket=WRITE_ACCOUNT, Key=f"{RESTRICTED_PRODUCT}/countries.parquet"
    )
    assert obj["ContentLength"] > 0
