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
gated on env vars and SKIPS when they are unset. The staging deploy workflow
(.github/workflows/staging.yml, job ``federation-smoke``) runs it against the
deployed worker once the ``FEDERATION_TEST_*`` repo variables are set:

  PROXY_URL                  base URL of the deployed proxy (shared with the other
                             integration tests; defaults to http://localhost:8787)
  FEDERATION_TEST_ACCOUNT    account id of the federated test product
  FEDERATION_TEST_PRODUCT    product id of the public federated test product
  FEDERATION_TEST_KEY        an object key expected to be readable via federation
  FEDERATION_RESTRICTED_PRODUCT
                             product id of a *restricted* federated product in the
                             same account, used by the authz/confused-deputy test
                             (an anonymous caller must be denied before federation
                             reads its private backend)
"""

import os

import pytest
import requests

PROXY_URL = os.environ.get("PROXY_URL", "http://localhost:8787")
ACCOUNT = os.environ.get("FEDERATION_TEST_ACCOUNT")
PRODUCT = os.environ.get("FEDERATION_TEST_PRODUCT")
KEY = os.environ.get("FEDERATION_TEST_KEY")
RESTRICTED_PRODUCT = os.environ.get("FEDERATION_RESTRICTED_PRODUCT")


@pytest.mark.skipif(
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
    # A 200 is the success signal; the body may legitimately be empty (a valid
    # zero-byte S3 object), so don't assert on resp.content.
    assert resp.status_code == 200, (
        f"federated read failed ({resp.status_code}): {resp.text[:300]}"
    )


@pytest.mark.skipif(
    not (ACCOUNT and RESTRICTED_PRODUCT),
    reason=(
        "restricted federation target not configured "
        "(set FEDERATION_TEST_ACCOUNT/FEDERATION_RESTRICTED_PRODUCT)"
    ),
)
def test_restricted_product_denied_to_anonymous():
    """An unauthorized caller must be denied *before* the proxy federates (#142).

    The confused-deputy guard: a restricted product's subject-scoped Source API
    lookup returns 403/404 for an anonymous caller, short-circuiting
    ``resolve_product`` before ``apply_backend_auth`` runs — so the proxy never
    assumes the role or serves the private backend on an unauthorized caller's
    behalf. A 200 here would mean federation leaked restricted data.

    The Source API maps unauthorized to AccessDenied (403) or, to avoid leaking
    existence, NotFound (404); both are acceptable denials. The key invariant is
    simply: not 200.
    """
    key = KEY or "any-key"
    resp = requests.get(f"{PROXY_URL}/{ACCOUNT}/{RESTRICTED_PRODUCT}/{key}")
    assert resp.status_code in (401, 403, 404), (
        "anonymous caller was not denied a restricted federated product "
        f"(status {resp.status_code}); federation may have served private data"
    )
