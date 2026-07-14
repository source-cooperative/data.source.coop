"""Authenticated write tests against the write-probe product (see stub_api.py).

Data requests to the proxy are SigV4-only (Bearer JWTs are rejected), so an
authenticated write follows the real client flow end-to-end:

  1. Obtain an OIDC identity token whose `aud` is in the worker's AUTH_AUDIENCE.
     In CI this is a GitHub Actions OIDC token (AUTH_ISSUER =
     https://token.actions.githubusercontent.com); the proxy verifies it via
     OIDC discovery against GitHub's JWKS.
  2. Exchange it at POST /.sts (AssumeRoleWithWebIdentity, RoleArn=_default)
     for temporary credentials whose SessionToken is sealed under
     SESSION_TOKEN_KEY.
  3. SigV4-sign S3 requests with those credentials; the proxy unseals the
     token, verifies the signature, and recovers the subject (the JWT's `sub`).

Two tiers, so the suite degrades gracefully:

  - anonymous write denial: always runs (proxy-side, no credentials involved).
  - /.sts exchange + SigV4 identity: needs CI_WRITE_ID_TOKEN (a GitHub OIDC
    token minted in ci.yml; absent on fork PRs and local runs -> skipped).
    Hermetic — no AWS infrastructure required.

CI's worker signs with a throwaway key on purpose, so real AWS federation can
never succeed here; that end of the path is covered by the deployed-environment
smoke tests (tests/test_federation.py, wired into staging.yml).
"""

import os
import xml.etree.ElementTree as ET

import pytest
import requests

from stub_api import WRITE_ACCOUNT, WRITE_PRODUCT

PROXY_URL = os.environ.get("PROXY_URL", "http://localhost:8787")
ID_TOKEN = os.environ.get("CI_WRITE_ID_TOKEN")

needs_token = pytest.mark.skipif(
    not ID_TOKEN,
    reason="caller identity not configured (set CI_WRITE_ID_TOKEN)",
)


def exchange_token():
    """POST /.sts and return (access_key_id, secret_access_key, session_token)."""
    resp = requests.post(
        f"{PROXY_URL}/.sts",
        params={
            "Action": "AssumeRoleWithWebIdentity",
            "RoleArn": "_default",
            "WebIdentityToken": ID_TOKEN,
        },
    )
    assert resp.status_code == 200, f"/.sts exchange failed ({resp.status_code}): {resp.text[:300]}"
    # Match by local name to stay independent of the response's xmlns.
    fields = {
        el.tag.rpartition("}")[2]: el.text
        for el in ET.fromstring(resp.text).iter()
    }
    return fields["AccessKeyId"], fields["SecretAccessKey"], fields["SessionToken"]


def s3_client():
    import boto3  # deferred: only the credentialed tests need it
    from botocore.config import Config

    access_key, secret_key, session_token = exchange_token()
    return boto3.client(
        "s3",
        endpoint_url=PROXY_URL,
        aws_access_key_id=access_key,
        aws_secret_access_key=secret_key,
        aws_session_token=session_token,
        region_name="us-east-1",
        config=Config(s3={"addressing_style": "path"}),
    )


def test_anonymous_write_to_federated_product_denied():
    """No credentials -> denied at the gate, before any backend/STS call."""
    resp = requests.put(f"{PROXY_URL}/{WRITE_ACCOUNT}/{WRITE_PRODUCT}/denied.txt")
    assert resp.status_code == 403


@needs_token
def test_sts_exchange_issues_credentials():
    """The proxy verifies the GitHub OIDC token (issuer discovery, aud check)
    and mints sealed temporary credentials. Hermetic — no AWS involved."""
    access_key, secret_key, session_token = exchange_token()
    assert access_key.startswith("STSPRXY")
    assert secret_key
    assert session_token


@needs_token
def test_sigv4_identity_is_recognized():
    """A SigV4-signed request with the sealed token resolves to an authenticated
    subject: listing a public product succeeds (rather than 403 on bad auth)."""
    import botocore.exceptions

    client = s3_client()
    try:
        client.list_objects_v2(Bucket="cholmes", Prefix="admin-boundaries/", MaxKeys=1)
    except botocore.exceptions.ClientError as e:
        pytest.fail(f"signed request rejected: {e}")
