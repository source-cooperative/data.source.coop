"""Static Source API stub for the CI integration tests.

The integration tests used to resolve products against the live prod API
(https://source.coop). That broke without any code change when the live
``aws-opendata-us-west-2`` data connection gained an ``s3_web_identity_role``:
the proxy then tries AWS STS federation, which CI's throwaway OIDC key can
never sign, and every object read 502s.

This stub serves the three control-plane endpoints the proxy fetches, with the
connection left *unsigned* so reads go to the (genuinely public) backing bucket
without credentials. The data plane still exercises the real bucket; only the
control plane is pinned. Responses carry just the fields the proxy
deserializes (see src/source_api/types.rs) — serde ignores the rest anyway.

CI starts this before `wrangler dev` and points the worker at it via
SOURCE_API_URL in .dev.vars.
"""

import json
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = 9000

# Must match the constants in test_integration.py and the real layout of the
# public bucket, since object reads hit it for real.
ACCOUNT = "cholmes"
PRODUCT = "admin-boundaries"
CONNECTION = "aws-opendata-us-west-2"

PRODUCT_JSON = {
    "product_id": PRODUCT,
    "disabled": False,
    "visibility": "public",
    "metadata": {
        "mirrors": {
            CONNECTION: {
                "connection_id": CONNECTION,
                "prefix": f"{ACCOUNT}/{PRODUCT}/",
            }
        },
        "primary_mirror": CONNECTION,
    },
}

ROUTES = {
    f"/api/v1/products/{ACCOUNT}": {"products": [PRODUCT_JSON]},
    f"/api/v1/products/{ACCOUNT}/{PRODUCT}": PRODUCT_JSON,
    # No `authentication` field -> BackendAuth::Unsigned -> unsigned reads.
    f"/api/v1/data-connections/{CONNECTION}": {
        "data_connection_id": CONNECTION,
        "read_only": False,
        "details": {
            "provider": "s3",
            "bucket": "us-west-2.opendata.source.coop",
            "region": "us-west-2",
            "base_prefix": "",
        },
    },
}


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        body = ROUTES.get(self.path.split("?")[0])
        self.send_response(200 if body else 404)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(body if body is not None else {}).encode())

    def log_message(self, *args):  # keep CI logs quiet
        pass


if __name__ == "__main__":
    print(f"source api stub listening on :{PORT}", flush=True)
    HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
