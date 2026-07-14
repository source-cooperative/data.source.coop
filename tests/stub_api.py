"""Static Source API stub for the CI integration tests.

The integration tests used to resolve products against the live prod API
(https://source.coop). That broke without any code change when the live
``aws-opendata-us-west-2`` data connection gained an ``s3_web_identity_role``:
the proxy then tries AWS STS federation, which CI's throwaway OIDC key can
never sign, and every object read 502s.

This stub serves the three control-plane endpoints the proxy fetches, with the
connection left *unsigned* so reads go to the (genuinely public) backing bucket
without credentials. The data plane still exercises the real bucket; only the
control plane is pinned.

Response bodies live in tests/fixtures/*.json, shared with tests/fixtures.rs,
which deserializes each one through the proxy's real serde structs
(src/source_api/types.rs) — so "the stub serves what the proxy parses" is a
compiled fact, not a comment. Fixtures carry just the fields the proxy
deserializes; serde ignores the rest anyway.

CI starts this before `wrangler dev` and points the worker at it via
SOURCE_API_URL in .dev.vars.
"""

import json
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

PORT = 9000

_FIXTURES = Path(__file__).parent / "fixtures"


def _fixture(name):
    return json.loads((_FIXTURES / f"{name}.json").read_text())


# Must match the constants in test_integration.py and the real layout of the
# public bucket, since object reads hit it for real.
ACCOUNT = "cholmes"
PRODUCT = "admin-boundaries"
CONNECTION = "aws-opendata-us-west-2"

PRODUCT_JSON = _fixture("product")

# ── Write probe ────────────────────────────────────────────────────
# A synthetic product on a federated (s3_web_identity_role) connection, used by
# test_writes.py for the proxy-side write path (anonymous denial, /.sts + SigV4
# identity). The bucket/role are deliberately unresolvable: CI's worker signs
# with a throwaway key, so AWS federation can never succeed here — the
# placeholder connection instead pins that federation failures stay fail-closed
# (see test_control_plane.py). Real federated e2e lives in test_federation.py
# against deployed environments. The stub does no per-subject authz: the
# permissions endpoint grants write to every authenticated subject.
WRITE_ACCOUNT = "ci-tests"
WRITE_PRODUCT = "write-probe"
WRITE_CONNECTION = "ci-write-probe"

WRITE_PRODUCT_JSON = _fixture("product_write_probe")

# ── Failure probes ─────────────────────────────────────────────────
# Synthetic products that make the control plane misbehave on purpose, so the
# proxy's fail-closed error mapping is exercised in CI (the original incident
# was an untested control-plane failure path). See test_control_plane.py.
ERR_500_PRODUCT = "err-500"
ERR_BAD_JSON_PRODUCT = "err-bad-json"

# A restricted product: the real API hides products a caller isn't entitled
# to (404, so existence doesn't leak). The stub's version of "entitled" is
# simply presenting an Authorization header — the proxy only sends one when
# it recovered an authenticated subject. Its mirror points at the same public
# bucket data as the read product, so an authorized read serves real bytes.
RESTRICTED_PRODUCT = "restricted-probe"
RESTRICTED_PRODUCT_JSON = _fixture("product_restricted")

ROUTES = {
    f"/api/v1/products/{ACCOUNT}": {"products": [PRODUCT_JSON]},
    f"/api/v1/products/{ACCOUNT}/{PRODUCT}": PRODUCT_JSON,
    # No `authentication` field -> BackendAuth::Unsigned -> unsigned reads.
    f"/api/v1/data-connections/{CONNECTION}": _fixture("data_connection"),
    # Write probe (see above).
    f"/api/v1/products/{WRITE_ACCOUNT}": {"products": [WRITE_PRODUCT_JSON]},
    f"/api/v1/products/{WRITE_ACCOUNT}/{WRITE_PRODUCT}": WRITE_PRODUCT_JSON,
    f"/api/v1/products/{WRITE_ACCOUNT}/{WRITE_PRODUCT}/permissions": ["read", "write"],
    f"/api/v1/data-connections/{WRITE_CONNECTION}": _fixture(
        "data_connection_write_probe"
    ),
}


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        path = self.path.split("?")[0]
        if path == f"/api/v1/products/{WRITE_ACCOUNT}/{ERR_500_PRODUCT}":
            return self._send(500, b"{}")
        if path == f"/api/v1/products/{WRITE_ACCOUNT}/{ERR_BAD_JSON_PRODUCT}":
            return self._send(200, b"{this is not json")
        if path == f"/api/v1/products/{WRITE_ACCOUNT}/{RESTRICTED_PRODUCT}":
            if self.headers.get("Authorization"):
                return self._send(200, json.dumps(RESTRICTED_PRODUCT_JSON).encode())
            return self._send(404, b"{}")
        body = ROUTES.get(path)
        if body is None:
            return self._send(404, b"{}")
        self._send(200, json.dumps(body).encode())

    def _send(self, status, body):
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(body)


if __name__ == "__main__":
    print(f"source api stub listening on :{PORT}", flush=True)
    HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
