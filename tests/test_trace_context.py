"""Trace context (ADR-016): every response names its trace in
x-source-trace-id, a caller's traceparent is continued, and Source API calls
carry it on.

Requires the worker at PROXY_URL and tests/stub_api.py at STUB_URL, as in CI.
"""

import os
import re
import uuid

import requests

PROXY_URL = os.environ.get("PROXY_URL", "http://localhost:8787")
STUB_URL = "http://localhost:9000"
ACCOUNT = "cholmes"
CALLER_SPAN = "00f067aa0ba902b7"


def _traceparent(trace_id, flags="01"):
    return f"00-{trace_id}-{CALLER_SPAN}-{flags}"


def test_a_response_names_the_callers_trace():
    trace_id = uuid.uuid4().hex
    resp = requests.get(f"{PROXY_URL}/", headers={"traceparent": _traceparent(trace_id)})
    assert resp.headers["x-source-trace-id"] == trace_id


def test_a_request_without_one_starts_its_own():
    first = requests.get(f"{PROXY_URL}/").headers["x-source-trace-id"]
    second = requests.get(f"{PROXY_URL}/").headers["x-source-trace-id"]
    assert re.fullmatch(r"[0-9a-f]{32}", first)
    assert first != second


def test_a_malformed_traceparent_starts_a_new_trace():
    resp = requests.get(f"{PROXY_URL}/", headers={"traceparent": "00-not-a-trace-01"})
    assert re.fullmatch(r"[0-9a-f]{32}", resp.headers["x-source-trace-id"])


def test_an_error_response_names_its_trace():
    trace_id = uuid.uuid4().hex
    resp = requests.get(
        f"{PROXY_URL}/{ACCOUNT}/no-such-product-{uuid.uuid4().hex[:8]}/key",
        headers={"traceparent": _traceparent(trace_id)},
    )
    assert resp.status_code == 404
    assert resp.headers["x-source-trace-id"] == trace_id


def test_source_api_calls_continue_the_trace():
    # A product the stub doesn't know: its 404 is never cached, so the
    # proxy asks the API every time.
    trace_id = uuid.uuid4().hex
    product = f"no-such-product-{uuid.uuid4().hex[:8]}"
    requests.get(
        f"{PROXY_URL}/{ACCOUNT}/{product}/key",
        headers={"traceparent": _traceparent(trace_id, flags="00")},
    )
    sent = requests.get(f"{STUB_URL}/_stub/product-lookup-traceparents").json()
    version, sent_trace, parent, flags = sent[f"/api/v1/products/{ACCOUNT}/{product}"].split("-")
    assert (version, sent_trace, flags) == ("00", trace_id, "00")
    # The API's parent is the proxy's own span, not the caller's.
    assert re.fullmatch(r"[0-9a-f]{16}", parent) and parent != CALLER_SPAN
