"""API-key exchange at /.sts (ADR-013), against the stub Source API.

A key is opaque: the proxy hashes it and asks the stub for its standing, as
itself, then mints credentials for the account the stub names. These tests
pin the parts that only run in the worker — the form-body-only rule, the
uniform refusal, the 60s standing cache, and fail-closed on an API error —
by counting how often each key's standing reaches the stub, and the ceiling of
the Role a key is exchanged for.
"""

import re
import uuid
import xml.etree.ElementTree as ET

import pytest
import requests

from stub_api import (
    ERR_500_KEY,
    KEY_ACCOUNT,
    LIVE_KEY,
    REVOKED_KEY,
    UNKNOWN_KEY,
    WRITE_ACCOUNT,
    _hash,
)

PROXY_URL = "http://localhost:8787"
STUB_URL = "http://localhost:9000"

# The worker takes its request id from `cf-ray`, which Cloudflare sets on
# every real request and `wrangler dev` does not; the tests supply one.
RAY = "ci-ray-0001"
REQUEST_ID = re.compile(r"\(request id [^)]+\)")


def exchange(key, *, in_query=False, role="arn:aws:iam::000000000000:role/_default"):
    params = {"Action": "AssumeRoleWithWebIdentity", "RoleArn": role, "WebIdentityToken": key}
    headers = {"cf-ray": RAY}
    if in_query:
        return requests.post(f"{PROXY_URL}/.sts", params=params, headers=headers)
    return requests.post(f"{PROXY_URL}/.sts", data=params, headers=headers)


def lookups(key):
    return requests.get(f"{STUB_URL}/_stub/key-exchange-counts").json().get(_hash(key), 0)


def sts_fields(resp):
    return {el.tag.rpartition("}")[2]: el.text for el in ET.fromstring(resp.text).iter()}


def test_a_live_key_is_exchanged_for_credentials_of_its_account():
    resp = exchange(LIVE_KEY)
    assert resp.status_code == 200, resp.text[:300]
    fields = sts_fields(resp)
    assert fields["AccessKeyId"].startswith("STSPRXY")
    assert fields["SessionToken"]
    assert fields["Expiration"]
    # The account lives inside the sealed session token; the response names
    # only the role. That the token was sealed for KEY_ACCOUNT is pinned by
    # tests/keys.rs, which can unseal it.
    assert fields["AssumedRoleId"] == "_default"


def test_a_stock_sdk_acquires_credentials_from_a_token_file_holding_the_key(tmp_path, monkeypatch):
    """The point of the design: an unmodified SDK, configured by environment
    alone, with the key saved to a file the way a user saves it — trailing
    newline and all."""
    import boto3

    token_file = tmp_path / "key"
    token_file.write_text(LIVE_KEY + "\n")
    for var in ("AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN", "AWS_PROFILE"):
        monkeypatch.delenv(var, raising=False)
    monkeypatch.setenv("AWS_CONFIG_FILE", str(tmp_path / "config"))
    monkeypatch.setenv("AWS_SHARED_CREDENTIALS_FILE", str(tmp_path / "credentials"))
    monkeypatch.setenv("AWS_ROLE_ARN", "arn:aws:iam::000000000000:role/_default")
    monkeypatch.setenv("AWS_WEB_IDENTITY_TOKEN_FILE", str(token_file))
    monkeypatch.setenv("AWS_ENDPOINT_URL_STS", f"{PROXY_URL}/.sts")
    monkeypatch.setenv("AWS_REGION", "us-east-1")

    creds = boto3.Session().get_credentials().get_frozen_credentials()
    assert creds.access_key.startswith("STSPRXY")
    assert creds.token


def test_the_standing_is_cached_so_a_second_exchange_never_reaches_the_api():
    exchange(LIVE_KEY)
    before = lookups(LIVE_KEY)
    assert exchange(LIVE_KEY).status_code == 200
    assert lookups(LIVE_KEY) == before, "second exchange within the TTL asked the API again"


def test_a_trailing_newline_in_the_token_file_is_harmless():
    assert exchange(LIVE_KEY + "\n").status_code == 200


def test_unknown_and_revoked_keys_are_refused_alike_with_a_request_id():
    answers = {name: exchange(key) for name, key in [("unknown", UNKNOWN_KEY), ("revoked", REVOKED_KEY)]}
    for name, resp in answers.items():
        assert resp.status_code == 400, name
        fields = sts_fields(resp)
        assert fields["Code"] == "InvalidIdentityToken", name
        assert fields["Message"] == f"API key was not accepted (request id {RAY})", name
        assert resp.headers.get("x-amzn-requestid") == RAY, name
    # Nothing in the body says which it was.
    assert REQUEST_ID.sub("", answers["unknown"].text) == REQUEST_ID.sub("", answers["revoked"].text)
    # Refusals are cached too: an unknown key costs one lookup a minute.
    before = lookups(UNKNOWN_KEY)
    exchange(UNKNOWN_KEY)
    assert lookups(UNKNOWN_KEY) == before


def test_a_key_in_the_query_string_is_refused_before_any_lookup():
    before = lookups(LIVE_KEY)
    resp = exchange(LIVE_KEY, in_query=True)
    assert resp.status_code == 400
    assert "request body" in sts_fields(resp)["Message"]
    assert lookups(LIVE_KEY) == before


def test_a_malformed_key_is_refused_locally():
    before = sum(requests.get(f"{STUB_URL}/_stub/key-exchange-counts").json().values())
    for bad in ["sck_tooshort", "sck_" + "x" * 44, "SCK_" + "L" * 43]:
        resp = exchange(bad)
        assert resp.status_code == 400, bad
        assert sts_fields(resp)["Code"] == "InvalidIdentityToken", bad
    assert sum(requests.get(f"{STUB_URL}/_stub/key-exchange-counts").json().values()) == before


def test_a_wrong_role_is_reported_as_such():
    resp = exchange(LIVE_KEY, role="arn:aws:iam::000000000000:role/nope")
    assert resp.status_code == 400
    assert sts_fields(resp)["Code"] == "MalformedPolicyDocument"


def test_read_only_refuses_a_write_before_anything_is_looked_up():
    """ReadOnly's ceiling is checked locally, ahead of every lookup, so it
    refuses a write with AccessDenied even for a product the API has never
    heard of. FullAccess, and ReadOnly reading, get past it to the product
    lookup, which finds no such product. (A write that reached the upstream
    would fail closed in CI anyway, so the lookup is where to tell them apart.)"""
    import boto3
    from botocore.config import Config
    from botocore.exceptions import ClientError

    def client(role):
        fields = sts_fields(exchange(LIVE_KEY, role=f"arn:aws:iam::000000000000:role/{role}"))
        return boto3.client(
            "s3",
            endpoint_url=PROXY_URL,
            aws_access_key_id=fields["AccessKeyId"],
            aws_secret_access_key=fields["SecretAccessKey"],
            aws_session_token=fields["SessionToken"],
            region_name="us-east-1",
            config=Config(s3={"addressing_style": "path"}),
        )

    def error_code(call):
        with pytest.raises(ClientError) as exc:
            call()
        return exc.value.response["Error"]["Code"]

    key = f"no-such-product-{uuid.uuid4().hex}/x.txt"
    read_only, full_access = client("ReadOnly"), client("FullAccess")
    put = {"Bucket": WRITE_ACCOUNT, "Key": key, "Body": b"x"}
    assert error_code(lambda: read_only.put_object(**put)) == "AccessDenied"
    assert error_code(lambda: full_access.put_object(**put)) == "NoSuchBucket"
    assert error_code(lambda: read_only.get_object(Bucket=WRITE_ACCOUNT, Key=key)) == "NoSuchBucket"


def test_an_api_failure_fails_closed_and_is_not_cached():
    first = exchange(ERR_500_KEY)
    assert first.status_code == 500
    assert sts_fields(first)["Code"] == "InternalError"
    before = lookups(ERR_500_KEY)
    exchange(ERR_500_KEY)
    assert lookups(ERR_500_KEY) == before + 1, "a failed lookup was cached"
