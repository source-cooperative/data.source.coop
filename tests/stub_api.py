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

import base64
import hashlib
import json
import os
import re
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from urllib.parse import unquote

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

WRITE_CONNECTION_JSON = _fixture("data_connection_write_probe")
# Activation plumbing: the same CI_WRITE_PROBE_* variables that un-skip
# test_multipart_roundtrip_hive_partition replace the fixture placeholders
# here, so the test gate and the served connection can't drift apart.
_details = WRITE_CONNECTION_JSON["details"]
_details["bucket"] = os.environ.get("CI_WRITE_PROBE_BUCKET", _details["bucket"])
_details["region"] = os.environ.get("CI_WRITE_PROBE_REGION", _details["region"])
WRITE_CONNECTION_JSON["authentication"]["role_arn"] = os.environ.get(
    "CI_WRITE_PROBE_ROLE_ARN", WRITE_CONNECTION_JSON["authentication"]["role_arn"]
)

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
    f"/api/v1/data-connections/{WRITE_CONNECTION}": WRITE_CONNECTION_JSON,
}

# Import-time drift guards: routes are keyed on the constants above, bodies
# carry fixture literals. A mismatch must crash the stub at startup (CI's
# readiness curl then fails loudly) instead of surfacing as a mystery
# NoSuchBucket mid-suite.
assert PRODUCT_JSON["product_id"] == PRODUCT
assert CONNECTION in PRODUCT_JSON["metadata"]["mirrors"]
assert WRITE_PRODUCT_JSON["product_id"] == WRITE_PRODUCT
assert WRITE_CONNECTION in WRITE_PRODUCT_JSON["metadata"]["mirrors"]
assert RESTRICTED_PRODUCT_JSON["product_id"] == RESTRICTED_PRODUCT
assert ROUTES[f"/api/v1/data-connections/{CONNECTION}"]["data_connection_id"] == CONNECTION
assert (
    ROUTES[f"/api/v1/data-connections/{WRITE_CONNECTION}"]["data_connection_id"]
    == WRITE_CONNECTION
)


# ── API keys ───────────────────────────────────────────────────────
# Opaque keys the proxy resolves by SHA-256 at POST
# /api/v1/service-account-keys/exchanges (ADR-013), as itself. The stub keys
# its answers on the hash of each constant, so test_api_keys.py presents the
# key and never the hash — exactly what the proxy is meant to send. A counter
# per hash lets the tests prove the proxy's 60s standing cache is doing its
# job: the second exchange of a key must not reach here.
LIVE_KEY = "sck_" + "L" * 43
REVOKED_KEY = "sck_" + "R" * 43
UNKNOWN_KEY = "sck_" + "U" * 43
ERR_500_KEY = "sck_" + "E" * 43
KEY_ACCOUNT = "ci-tests--nightly-sync"


def _hash(key):
    return hashlib.sha256(key.encode()).hexdigest()


KEY_STANDINGS = {
    _hash(LIVE_KEY): (200, {"account_id": KEY_ACCOUNT, "key_id": "k-live", "active": True}),
    _hash(REVOKED_KEY): (200, {"active": False}),
    _hash(ERR_500_KEY): (500, {}),
}
KEY_EXCHANGE_COUNTS = {}


# ── Account trusts ─────────────────────────────────────────────────
# Whether an account trusts a platform token's issuer and subject, at POST
# /api/v1/accounts/{account}/trusts/exchanges (ADR-014). The proxy asks as the
# account itself. Only TRUST_ACCOUNT trusts anyone: GitHub Actions workflows
# in this repository, whatever event minted the token. A counter per account
# lets test_platform_trust.py prove the proxy caches a yes.
TRUST_ACCOUNT = "ci-tests--github-ci"
TRUSTED_ISSUER = "https://token.actions.githubusercontent.com"
TRUSTED_SUBJECT_PREFIX = "repo:source-cooperative/data.source.coop:"
TRUST_EXCHANGE_COUNTS = {}

# Who the proxy said it was asking as, per product path, so a test can check
# which principal a session carries: see test_platform_trust.py.
PRODUCT_LOOKUP_SUBJECTS = {}


def _bearer_subject(authorization):
    """The `sub` of the proxy's assertion, unverified: the stub has no key."""
    try:
        payload = authorization.removeprefix("Bearer ").split(".")[1]
        return json.loads(base64.urlsafe_b64decode(payload + "=" * (-len(payload) % 4)))["sub"]
    except (IndexError, ValueError, KeyError):
        return None


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        path = self.path.split("?")[0]
        trust = re.fullmatch(r"/api/v1/accounts/([^/]+)/trusts/exchanges", path)
        if trust:
            return self._trust_exchange(unquote(trust.group(1)))
        if path != "/api/v1/service-account-keys/exchanges":
            return self._send(404, b"{}")
        # The proxy authenticates as itself; the stub cannot verify the
        # signature, but a missing header would mean the proxy sent nothing.
        if not self.headers.get("Authorization", "").startswith("Bearer "):
            return self._send(401, b"{}")
        length = int(self.headers.get("content-length") or 0)
        try:
            key_hash = json.loads(self.rfile.read(length))["key_hash"]
        except (ValueError, KeyError, TypeError):
            return self._send(400, b"{}")
        KEY_EXCHANGE_COUNTS[key_hash] = KEY_EXCHANGE_COUNTS.get(key_hash, 0) + 1
        status, body = KEY_STANDINGS.get(key_hash, (200, {"active": False}))
        self._send(status, json.dumps(body).encode())

    def _trust_exchange(self, account):
        # Anyone but the account itself is refused, as the real route does.
        if _bearer_subject(self.headers.get("Authorization", "")) != account:
            return self._send(401, b'{"error": "Unauthorized"}')
        length = int(self.headers.get("content-length") or 0)
        try:
            body = json.loads(self.rfile.read(length))
            issuer, subject = body["issuer"], body["subject"]
        except (ValueError, KeyError, TypeError):
            return self._send(400, b"{}")
        TRUST_EXCHANGE_COUNTS[account] = TRUST_EXCHANGE_COUNTS.get(account, 0) + 1
        trusted = (
            account == TRUST_ACCOUNT
            and issuer == TRUSTED_ISSUER
            and subject.startswith(TRUSTED_SUBJECT_PREFIX)
        )
        self._send(200 if trusted else 403, json.dumps({"trusted": trusted}).encode())

    def do_GET(self):
        path = self.path.split("?")[0]
        # Test-only: how many times each key's standing was asked for.
        if path == "/_stub/key-exchange-counts":
            return self._send(200, json.dumps(KEY_EXCHANGE_COUNTS).encode())
        # Test-only: how many times each account's trust was asked for.
        if path == "/_stub/trust-exchange-counts":
            return self._send(200, json.dumps(TRUST_EXCHANGE_COUNTS).encode())
        # Test-only: the subject each product was last looked up as.
        if path == "/_stub/product-lookup-subjects":
            return self._send(200, json.dumps(PRODUCT_LOOKUP_SUBJECTS).encode())
        if path.startswith("/api/v1/products/") and self.headers.get("Authorization"):
            PRODUCT_LOOKUP_SUBJECTS[path] = _bearer_subject(self.headers["Authorization"])
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
