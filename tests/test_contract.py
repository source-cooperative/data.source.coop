"""Contract tests: the stub API must remain shape-compatible with the real one.

The stub (stub_api.py) pins the control plane so integration tests can't be
broken by live drift — but that also makes them blind to it. These tests close
the loop: every field the stub serves (which is, by construction, every field
the proxy deserializes — see src/source_api/types.rs) must still exist in the
real Source API's responses with the same JSON type. Extra real fields are fine
(serde ignores them); a missing or retyped field means the stub is lying about
the contract and both it and types.rs need updating together.

The write-probe permissions endpoint has no anonymously-fetchable real
counterpart, so it is not contract-tested here; its shape is covered by the
authz behavior of the deployed-environment smoke tests instead.

These tests hit the live prod API, so they run nightly
(.github/workflows/contract.yml), not in the PR-gating CI job — a prod outage
must not redden unrelated PRs.
"""

import requests

from stub_api import ACCOUNT, CONNECTION, PRODUCT, PRODUCT_JSON, ROUTES

REAL_API = "https://source.coop"

# BackendAuth variants the proxy implements (src/backend_auth.rs). Anything
# else deserializes to Unsupported and the proxy fails closed on the whole
# connection — exactly the drift we want an early warning for.
KNOWN_AUTH_TYPES = {"unsigned", "s3_web_identity_role"}


def fetch(path):
    resp = requests.get(f"{REAL_API}{path}", timeout=30)
    assert resp.status_code == 200, f"real API returned {resp.status_code} for {path}"
    return resp.json()


def json_type(value):
    # bool before int: bool subclasses int in Python but is a distinct JSON type.
    if isinstance(value, bool):
        return "boolean"
    if isinstance(value, (int, float)):
        return "number"
    if isinstance(value, str):
        return "string"
    if isinstance(value, dict):
        return "object"
    if isinstance(value, list):
        return "array"
    return "null"


def assert_shape_subset(stub, real, path):
    """Every stub key exists in the real response with the same JSON type."""
    assert json_type(stub) == json_type(real), (
        f"{path}: stub is {json_type(stub)} but real API returned {json_type(real)}"
    )
    if isinstance(stub, dict):
        for key, stub_value in stub.items():
            assert key in real, f"{path}.{key}: missing from real API response"
            assert_shape_subset(stub_value, real[key], f"{path}.{key}")


def test_product_shape():
    real = fetch(f"/api/v1/products/{ACCOUNT}/{PRODUCT}")
    assert_shape_subset(PRODUCT_JSON, real, "product")


def test_product_list_shape():
    real = fetch(f"/api/v1/products/{ACCOUNT}")
    stub = ROUTES[f"/api/v1/products/{ACCOUNT}"]
    assert json_type(real.get("products")) == "array", "products list missing"
    matches = [p for p in real["products"] if p.get("product_id") == PRODUCT]
    assert matches, f"product {PRODUCT} missing from real list response"
    assert_shape_subset(stub["products"][0], matches[0], "products[]")


def test_data_connection_shape():
    real = fetch(f"/api/v1/data-connections/{CONNECTION}")
    stub = ROUTES[f"/api/v1/data-connections/{CONNECTION}"]
    assert_shape_subset(stub, real, "connection")


def test_data_connection_auth_is_supported():
    """The real connection's authentication must parse to a known BackendAuth
    variant — an unknown type would make the proxy fail closed on every read."""
    real = fetch(f"/api/v1/data-connections/{CONNECTION}")
    auth = real.get("authentication")
    if auth is None:
        return  # absent/null -> Unsigned, fine
    assert json_type(auth) == "object", f"authentication is {json_type(auth)}"
    assert auth.get("type") in KNOWN_AUTH_TYPES, (
        f"unknown authentication type {auth.get('type')!r}: the proxy would "
        "fail closed on this connection (BackendAuth::Unsupported)"
    )
    if auth["type"] == "s3_web_identity_role":
        assert json_type(auth.get("role_arn")) == "string", (
            "s3_web_identity_role without a string role_arn deserializes to "
            "Unsupported and fails closed"
        )
