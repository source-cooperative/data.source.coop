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
import zlib
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlsplit

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
    f"/api/v1/products/{ACCOUNT}": {"items": [PRODUCT_JSON], "next_cursor": None},
    f"/api/v1/products/{ACCOUNT}/{PRODUCT}": PRODUCT_JSON,
    # No `authentication` field -> BackendAuth::Unsigned -> unsigned reads.
    f"/api/v1/data-connections/{CONNECTION}": _fixture("data_connection"),
    # Write probe (see above).
    f"/api/v1/products/{WRITE_ACCOUNT}/{WRITE_PRODUCT}": WRITE_PRODUCT_JSON,
    f"/api/v1/products/{WRITE_ACCOUNT}/{WRITE_PRODUCT}/permissions": ["read", "write"],
    f"/api/v1/data-connections/{WRITE_CONNECTION}": WRITE_CONNECTION_JSON,
}

# The write account's listing spans two pages, so the proxy has to follow
# next_cursor to list both products. The cursor holds the characters a real
# (base64) one does, which reach here intact only if the proxy encodes them:
# an unencoded "+" arrives as a space.
SECOND_PAGE_PRODUCT = "second-page"
SECOND_PAGE_CURSOR = "eyJw+/Ig=="
PRODUCT_LIST_PAGES = {
    None: {"items": [WRITE_PRODUCT_JSON], "next_cursor": SECOND_PAGE_CURSOR},
    SECOND_PAGE_CURSOR: {
        "items": [{**WRITE_PRODUCT_JSON, "product_id": SECOND_PAGE_PRODUCT}],
        "next_cursor": None,
    },
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
BASE62 = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"


def _key(char):
    """A well-formed key: 30 of `char`, then their CRC-32 in six base62 digits."""
    body, n, checksum = char * 30, zlib.crc32((char * 30).encode()), ""
    for _ in range(6):
        n, digit = divmod(n, 62)
        checksum = BASE62[digit] + checksum
    return "sck_" + body + checksum


LIVE_KEY = _key("L")
REVOKED_KEY = _key("R")
UNKNOWN_KEY = _key("U")
ERR_500_KEY = _key("E")
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
# account itself. Only TRUST_ACCOUNT trusts anyone, and only the exact subject
# of the token CI minted for this run (CI_TRUSTED_SUBJECT, set by ci.yml), as
# source.coop matches a trust exactly. SAYS_NO_ACCOUNT answers no with a 200,
# which the real route never does, so the proxy's read of the body is pinned.
# A counter per account lets test_platform_trust.py prove the proxy caches.
TRUST_ACCOUNT = "ci-tests--github-ci"
SAYS_NO_ACCOUNT = "ci-tests--says-no-with-200"
TRUSTED_ISSUER = "https://token.actions.githubusercontent.com"
TRUSTED_SUBJECT = os.environ.get("CI_TRUSTED_SUBJECT")
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


# source.coop answers every error as an RFC 9457 problem; the proxy reads only
# the status, so the stub's problems carry no more than the shape.
PROBLEMS = {
    "invalid": (400, "Invalid request"),
    "unauthenticated": (401, "Authentication required"),
    "forbidden": (403, "Forbidden"),
    "not_found": (404, "Not found"),
    "internal": (500, "Internal server error"),
}


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        path = self.path.split("?")[0]
        trust = re.fullmatch(r"/api/v1/accounts/([^/]+)/trusts/exchanges", path)
        if trust:
            return self._trust_exchange(unquote(trust.group(1)))
        if path != "/api/v1/service-account-keys/exchanges":
            return self._problem("not_found", f"No API route at {path}")
        # The proxy authenticates as itself; the stub cannot verify the
        # signature, but a missing header would mean the proxy sent nothing.
        if not self.headers.get("Authorization", "").startswith("Bearer "):
            return self._problem("unauthenticated", "Authentication required")
        length = int(self.headers.get("content-length") or 0)
        try:
            key_hash = json.loads(self.rfile.read(length))["key_hash"]
        except (ValueError, KeyError, TypeError):
            return self._problem("invalid", "key_hash is required")
        KEY_EXCHANGE_COUNTS[key_hash] = KEY_EXCHANGE_COUNTS.get(key_hash, 0) + 1
        status, body = KEY_STANDINGS.get(key_hash, (200, {"active": False}))
        if status == 500:
            return self._problem("internal", "Internal server error")
        self._send(status, json.dumps(body).encode())

    def _trust_exchange(self, account):
        # Anyone but the account itself is refused, as the real route does.
        if _bearer_subject(self.headers.get("Authorization", "")) != account:
            return self._problem("unauthenticated", "Authentication required")
        length = int(self.headers.get("content-length") or 0)
        try:
            body = json.loads(self.rfile.read(length))
            issuer, subject = body["issuer"], body["subject"]
        except (ValueError, KeyError, TypeError):
            return self._problem("invalid", "issuer and subject are required")
        TRUST_EXCHANGE_COUNTS[account] = TRUST_EXCHANGE_COUNTS.get(account, 0) + 1
        if account == SAYS_NO_ACCOUNT:
            return self._send(200, b'{"trusted": false}')
        trusted = (
            account == TRUST_ACCOUNT
            and issuer == TRUSTED_ISSUER
            and subject == TRUSTED_SUBJECT
        )
        if not trusted:
            # A `forbidden` problem that also says no, as the real route answers.
            return self._problem(
                "forbidden", f"{account} does not trust this subject", trusted=False
            )
        self._send(200, json.dumps({"trusted": True}).encode())

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
            return self._problem("internal", "Internal server error")
        if path == f"/api/v1/products/{WRITE_ACCOUNT}/{ERR_BAD_JSON_PRODUCT}":
            return self._send(200, b"{this is not json")
        if path == f"/api/v1/products/{WRITE_ACCOUNT}/{RESTRICTED_PRODUCT}":
            if self.headers.get("Authorization"):
                return self._send(200, json.dumps(RESTRICTED_PRODUCT_JSON).encode())
            return self._problem("not_found", "Product not found")
        if path == f"/api/v1/products/{WRITE_ACCOUNT}":
            cursor = parse_qs(urlsplit(self.path).query).get("cursor", [None])[0]
            body = PRODUCT_LIST_PAGES.get(cursor)
        else:
            body = ROUTES.get(path)
        if body is None:
            return self._problem("not_found", f"Nothing at {path}")
        self._send(200, json.dumps(body).encode())

    def _send(self, status, body, content_type="application/json"):
        self.send_response(status)
        self.send_header("content-type", content_type)
        self.end_headers()
        self.wfile.write(body)

    def _problem(self, code, detail, **extensions):
        """An error as source.coop answers every one: an RFC 9457 problem."""
        status, title = PROBLEMS[code]
        body = {
            "type": f"https://source.coop/api/problems/{code}",
            "title": title,
            "status": status,
            "detail": detail,
            "instance": self.path.split("?")[0],
            "code": code,
            **extensions,
        }
        self._send(status, json.dumps(body).encode(), "application/problem+json")


if __name__ == "__main__":
    print(f"source api stub listening on :{PORT}", flush=True)
    HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
