"""Federation smoke test for the Source Cooperative Data Proxy.

Exercises the full federated backend path end-to-end: a request for a product
backed by an ``s3_web_identity_role`` data connection makes the proxy mint its
own OIDC assertion, assume the customer role via AWS STS
``AssumeRoleWithWebIdentity``, and sign the S3 read with the temporary
credentials.

This needs real infrastructure that can't be stood up in unit tests — a deployed
proxy, a federated test product whose data connection carries a ``role_arn``, and
the customer-side IAM OIDC provider + role trust policy (conditioned on
``aud = sts.amazonaws.com`` and ``sub = scv1:conn:{connection_id}``). So it is
gated on env vars and SKIPS when they are unset. Set them in staging/preview CI
to activate it (it is discovered automatically by ``pytest tests/``):

  PROXY_URL                base URL of the deployed proxy (shared with the other
                           integration tests; defaults to http://localhost:8787)
  FEDERATION_TEST_ACCOUNT  account id of the federated test product
  FEDERATION_TEST_PRODUCT  product id of the federated test product
  FEDERATION_TEST_KEY      an object key expected to be readable via federation
"""

import os

import pytest
import requests

PROXY_URL = os.environ.get("PROXY_URL", "http://localhost:8787")
ACCOUNT = os.environ.get("FEDERATION_TEST_ACCOUNT")
PRODUCT = os.environ.get("FEDERATION_TEST_PRODUCT")
KEY = os.environ.get("FEDERATION_TEST_KEY")

pytestmark = pytest.mark.skipif(
    not (ACCOUNT and PRODUCT and KEY),
    reason=(
        "federation test target not configured "
        "(set FEDERATION_TEST_ACCOUNT/PRODUCT/KEY against a deployed proxy)"
    ),
)


def test_federated_object_is_served():
    """A private, federated product's object is served via AssumeRoleWithWebIdentity.

    A 403/500 here means the proxy could not assume the role or sign the request
    — typically a missing IAM OIDC provider, a trust-policy ``aud``/``sub``
    mismatch, or the API not surfacing the connection's ``role_arn`` to the proxy.
    """
    resp = requests.get(f"{PROXY_URL}/{ACCOUNT}/{PRODUCT}/{KEY}")
    assert resp.status_code == 200, (
        f"federated read failed ({resp.status_code}): {resp.text[:300]}"
    )
    assert resp.content, "federated read returned an empty body"
