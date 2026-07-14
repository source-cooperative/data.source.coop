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

import functools
import os
import uuid
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
needs_wrong_aud_token = pytest.mark.skipif(
    not WRONG_AUD_TOKEN and not _token_expected,
    reason="wrong-audience token not configured (set CI_WRONG_AUDIENCE_TOKEN)",
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


@functools.lru_cache(maxsize=1)
def exchange_token():
    """POST /.sts and return (access_key_id, secret_access_key, session_token).

    Cached: one real exchange serves every positive-path test instead of ~8
    identical OIDC verifications per run (the negatives call sts_exchange
    directly and stay uncached)."""
    resp = sts_exchange(ID_TOKEN)
    assert resp.status_code == 200, f"/.sts exchange failed ({resp.status_code}): {resp.text[:300]}"
    # Match by local name to stay independent of the response's xmlns.
    fields = {
        el.tag.rpartition("}")[2]: el.text
        for el in ET.fromstring(resp.text).iter()
    }
    return fields["AccessKeyId"], fields["SecretAccessKey"], fields["SessionToken"]


def s3_client(session_token=None, **config_kwargs):
    """Credentialed S3 client via /.sts; `session_token` overrides the sealed
    token so tests can present corrupted ones. `config_kwargs` merge into the
    botocore Config (e.g. retries)."""
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
        config=Config(s3={"addressing_style": "path"}, **config_kwargs),
    )


# Hive-style partition key (`country_iso=ETH`): the raw-signed multipart path
# where the 0.6.3 encoding bug (#180) lived. Only the round-trip test below
# reaches that encoding seam — the denial and fail-closed layers die at the
# authz gate / federation first; their hive keys just keep the paths uniform.
HIVE_KEY_DIR = "by_country/country_iso=ETH"


@pytest.mark.parametrize(
    "method,path",
    [
        ("put", "denied.txt"),
        ("post", f"{HIVE_KEY_DIR}/denied.pmtiles?uploads"),
    ],
    ids=["put", "multipart-create"],
)
def test_anonymous_write_to_federated_product_denied(method, path):
    """No credentials -> denied at the gate, before any backend/STS call.
    Every write op shares the gate; multipart create is one more of them."""
    resp = getattr(requests, method)(
        f"{PROXY_URL}/{WRITE_ACCOUNT}/{WRITE_PRODUCT}/{path}"
    )
    assert resp.status_code == 403


needs_probe_infra = pytest.mark.skipif(
    not os.environ.get("CI_WRITE_PROBE_ROLE_ARN"),
    reason="write-probe AWS infra not configured (set CI_WRITE_PROBE_ROLE_ARN)",
)


@needs_token
@needs_probe_infra
def test_multipart_roundtrip_hive_partition():
    """The proxy-level #180 guarantee: multipart create/upload/complete on a
    hive-partitioned key, read back and verified — the only test that reaches
    the raw-signed backend-URL encoding seam end-to-end.

    Activation (see #183's follow-ups): the probe bucket + IAM role, a worker
    signing key whose issuer the role's OIDC provider trusts (a dedicated CI
    key — not staging's), and ci.yml exporting the CI_WRITE_PROBE_* vars to
    both the stub and pytest. The stub derives its served bucket/role from
    the same variables (see stub_api.py), so this gate and the served config
    can't drift apart.
    """
    client = s3_client()
    key = (
        f"{WRITE_PRODUCT}/{HIVE_KEY_DIR}/"
        f"gh-{os.environ.get('GITHUB_RUN_ID', 'local')}-{uuid.uuid4().hex}.pmtiles"
    )
    bodies = (b"a" * (5 * 1024 * 1024), b"b" * 1024)  # 5 MiB min non-final part

    upload_id = client.create_multipart_upload(Bucket=WRITE_ACCOUNT, Key=key)[
        "UploadId"
    ]
    try:
        parts = []
        for n, body in enumerate(bodies, 1):
            resp = client.upload_part(
                Bucket=WRITE_ACCOUNT, Key=key, UploadId=upload_id,
                PartNumber=n, Body=body,
            )
            # Carry the Checksum* members through. Not required today (no
            # ChecksumAlgorithm was declared at create, so S3 validates only
            # PartNumber+ETag), but the moment one is declared — e.g. by
            # switching to boto3's transfer manager — S3 rejects a Complete
            # that omits per-part checksums. Harmless now, load-bearing then.
            parts.append(
                {"PartNumber": n, "ETag": resp["ETag"]}
                | {k: v for k, v in resp.items() if k.startswith("Checksum")}
            )
        client.complete_multipart_upload(
            Bucket=WRITE_ACCOUNT, Key=key, UploadId=upload_id,
            MultipartUpload={"Parts": parts},
        )
        got = client.get_object(Bucket=WRITE_ACCOUNT, Key=key)["Body"].read()
        assert got == b"".join(bodies)
    finally:
        # Best effort, order-independent: delete covers a completed object
        # (even one completed server-side after a client-side error), abort
        # covers an unfinished upload; the bucket lifecycle rule backstops.
        for cleanup in (
            lambda: client.delete_object(Bucket=WRITE_ACCOUNT, Key=key),
            lambda: client.abort_multipart_upload(
                Bucket=WRITE_ACCOUNT, Key=key, UploadId=upload_id
            ),
        ):
            try:
                cleanup()
            except Exception:
                pass


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


@needs_wrong_aud_token
def test_sts_rejects_wrong_audience():
    """A validly-signed token whose aud isn't in AUTH_AUDIENCE must be
    rejected — this is the gate that keeps other GitHub OIDC consumers'
    tokens from minting credentials here."""
    # Presence assert, not just the skipif: with the token missing,
    # sts_exchange(None) sends no WebIdentityToken and the 4xx assertion
    # below would pass vacuously — testing nothing.
    assert WRONG_AUD_TOKEN, (
        "CI_WRONG_AUDIENCE_TOKEN expected on same-repo runs but missing — "
        "did the env var name drift between ci.yml and this file?"
    )
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
@pytest.mark.parametrize("op", ["put_object", "create_multipart_upload"])
def test_federated_write_fails_closed(op):
    """The write probe's role is unassumable by design (placeholder ARN,
    throwaway signing key): backend STS federation must fail with a bounded,
    parseable S3 error — never a hang, never a silent success. This is the
    incident class that motivated the stub, kept permanently exercised.
    Federation errors before any backend URL is built, so this pins the
    fail-closed seam for every write op; key encoding is the round-trip
    test's job."""
    import botocore.exceptions

    # max_attempts=1: botocore's default mode retries the 502/503 this
    # produces, multiplying live AWS STS round-trips for no extra signal.
    client = s3_client(retries={"max_attempts": 1})
    kwargs = {
        "Bucket": WRITE_ACCOUNT,
        "Key": f"{WRITE_PRODUCT}/{HIVE_KEY_DIR}/fail-closed.pmtiles",
    }
    if op == "put_object":
        kwargs["Body"] = b"x"
    with pytest.raises(botocore.exceptions.ClientError) as exc:
        getattr(client, op)(**kwargs)
    code = exc.value.response["Error"].get("Code")
    assert code, "unparseable error response"
    # Federation failure maps only to AccessDenied / BackendAuthenticationFailed
    # / ServiceUnavailable / InternalError; the inbound-auth codes would mean
    # verification broke before federation was even attempted.
    assert code not in {"SignatureDoesNotMatch", "InvalidRequest"}, (
        f"{code}: rejected before the federation seam this test pins"
    )


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
