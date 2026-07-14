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
WRONG_AUD_TOKEN = os.environ.get("CI_WRONG_AUDIENCE_TOKEN")

# When CI declares a token must exist (same-repo runs export CI_EXPECT_OIDC),
# a missing token means the mint->env plumbing broke: run the tests and fail
# loudly instead of skipping the whole credentialed tier green.
_token_expected = os.environ.get("CI_EXPECT_OIDC") == "true"
needs_token = pytest.mark.skipif(
    not ID_TOKEN and not _token_expected,
    reason="caller identity not configured (set CI_WRITE_ID_TOKEN)",
)


def sts_exchange(token):
    """POST /.sts with the given web identity token; return the raw response."""
    return requests.post(
        f"{PROXY_URL}/.sts",
        params={
            "Action": "AssumeRoleWithWebIdentity",
            "RoleArn": "_default",
            "WebIdentityToken": token,
        },
    )


def exchange_token():
    """POST /.sts and return (access_key_id, secret_access_key, session_token)."""
    resp = sts_exchange(ID_TOKEN)
    assert resp.status_code == 200, f"/.sts exchange failed ({resp.status_code}): {resp.text[:300]}"
    # Match by local name to stay independent of the response's xmlns.
    fields = {
        el.tag.rpartition("}")[2]: el.text
        for el in ET.fromstring(resp.text).iter()
    }
    return fields["AccessKeyId"], fields["SecretAccessKey"], fields["SessionToken"]


def s3_client(session_token=None):
    """Credentialed S3 client via /.sts; `session_token` overrides the sealed
    token so tests can present corrupted ones."""
    import boto3  # deferred: only the credentialed tests need it
    from botocore.config import Config

    access_key, secret_key, real_token = exchange_token()
    return boto3.client(
        "s3",
        endpoint_url=PROXY_URL,
        aws_access_key_id=access_key,
        aws_secret_access_key=secret_key,
        aws_session_token=session_token or real_token,
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
    subject: listing a public product succeeds (rather than 403 on bad auth).

    The non-empty Prefix matters: it folds the request into a product-scoped
    list, which routes past the credential-blind account-list handler into the
    SigV4-verified pipeline. Without it this test wouldn't touch auth at all."""
    import botocore.exceptions

    client = s3_client()
    try:
        client.list_objects_v2(Bucket="cholmes", Prefix="admin-boundaries/", MaxKeys=1)
    except botocore.exceptions.ClientError as e:
        pytest.fail(f"signed request rejected: {e}")


# ── Negatives: the positive tests above would also pass against a fail-open
# verifier (a 200 is a 200), so each rejection path gets pinned explicitly. ──


def test_sts_rejects_garbage_token():
    """An unparseable web identity token must never mint credentials."""
    resp = sts_exchange("not-a-jwt")
    assert 400 <= resp.status_code < 500, (
        f"garbage token was not rejected ({resp.status_code}): {resp.text[:300]}"
    )


@needs_token
def test_sts_rejects_tampered_signature():
    """A real token with a corrupted signature must fail JWKS verification."""
    tampered = ID_TOKEN[:-1] + ("A" if ID_TOKEN[-1] != "A" else "B")
    resp = sts_exchange(tampered)
    assert 400 <= resp.status_code < 500, (
        f"tampered token was not rejected ({resp.status_code}): {resp.text[:300]}"
    )


@pytest.mark.skipif(
    not WRONG_AUD_TOKEN,
    reason="wrong-audience token not configured (set CI_WRONG_AUDIENCE_TOKEN)",
)
def test_sts_rejects_wrong_audience():
    """A validly-signed token whose aud isn't in AUTH_AUDIENCE must be
    rejected — this is the gate that keeps other GitHub OIDC consumers'
    tokens from minting credentials here."""
    resp = sts_exchange(WRONG_AUD_TOKEN)
    assert 400 <= resp.status_code < 500, (
        f"wrong-audience token was not rejected ({resp.status_code}): {resp.text[:300]}"
    )


@needs_token
def test_signed_request_with_special_char_key_verifies():
    """#176 regression pin: inbound SigV4 verification must use the encoded
    request path. A key with spaces and `*%~#+` must fail as 404 (no such
    key), never 403 SignatureDoesNotMatch."""
    import botocore.exceptions

    client = s3_client()
    with pytest.raises(botocore.exceptions.ClientError) as exc:
        client.head_object(
            Bucket="cholmes", Key="admin-boundaries/no such key *%~#+.txt"
        )
    status = exc.value.response["ResponseMetadata"]["HTTPStatusCode"]
    assert status == 404, (
        f"expected 404 for a nonexistent key, got {status} — a 403 here means "
        "inbound signature verification broke on encoded paths again"
    )


@needs_token
def test_federated_write_fails_closed():
    """The write probe's role is unassumable by design (placeholder ARN,
    throwaway signing key): backend STS federation must fail with a bounded,
    parseable S3 error — never a hang, never a silent success. This is the
    incident class that motivated the stub, kept permanently exercised."""
    import botocore.exceptions

    client = s3_client()
    with pytest.raises(botocore.exceptions.ClientError) as exc:
        client.put_object(
            Bucket=WRITE_ACCOUNT, Key=f"{WRITE_PRODUCT}/fail-closed.txt", Body=b"x"
        )
    # The exact code depends on how AWS STS rejects the assertion; what
    # matters is that boto3 could parse an S3-shaped error at all.
    assert exc.value.response["Error"].get("Code"), "unparseable error response"


@needs_token
def test_corrupted_session_token_rejected():
    """A SigV4 request whose sealed SessionToken has been tampered with must
    be rejected, not fall back to anonymous-and-succeed on a public product."""
    import botocore.exceptions

    good = exchange_token()[2]
    corrupted = good[:-4] + ("AAAA" if good[-4:] != "AAAA" else "BBBB")
    client = s3_client(session_token=corrupted)
    # An object GET, not a list: object paths route through the SigV4-verified
    # pipeline unconditionally, so this can't silently degrade into hitting a
    # credential-blind handler.
    with pytest.raises(botocore.exceptions.ClientError) as exc:
        client.get_object(Bucket="cholmes", Key="admin-boundaries/countries.parquet")
    status = exc.value.response["ResponseMetadata"]["HTTPStatusCode"]
    assert status in (400, 403), f"expected 400/403, got {status}"
