"""The person issuer's STS route, with a validly signed token.

CI's main worker has no person issuer that mints anything. A second worker
(PERSON_PROXY_URL in ci.yml) names GitHub Actions as AUTH_ISSUER instead, so
CI's GitHub token is a person token there: it exchanges at `_default` and acts
as its own subject. That worker's PLATFORM_ISSUERS also names GitHub, which
the proxy drops at load, so these tests also pin that the person route wins.
"""

import os
import xml.etree.ElementTree as ET

import pytest
import requests

from test_writes import ID_TOKEN, WRONG_AUD_TOKEN

PERSON_PROXY_URL = os.environ.get("PERSON_PROXY_URL")

pytestmark = pytest.mark.skipif(
    not PERSON_PROXY_URL, reason="person-issuer worker not configured (set PERSON_PROXY_URL)"
)


def exchange(token):
    params = {"Action": "AssumeRoleWithWebIdentity", "RoleArn": "_default", "WebIdentityToken": token}
    return requests.post(f"{PERSON_PROXY_URL}/.sts", data=params)


def test_a_person_token_gets_credentials_that_sign():
    assert ID_TOKEN, "PERSON_PROXY_URL is set but CI_WRITE_ID_TOKEN is not"
    import boto3
    from botocore.config import Config

    resp = exchange(ID_TOKEN)
    assert resp.status_code == 200, resp.text[:300]
    fields = {el.tag.rpartition("}")[2]: el.text for el in ET.fromstring(resp.text).iter()}
    client = boto3.client(
        "s3",
        endpoint_url=PERSON_PROXY_URL,
        aws_access_key_id=fields["AccessKeyId"],
        aws_secret_access_key=fields["SecretAccessKey"],
        aws_session_token=fields["SessionToken"],
        region_name="us-east-1",
        config=Config(s3={"addressing_style": "path"}),
    )
    # A product-scoped list runs SigV4 verification, which unseals the token.
    client.list_objects_v2(Bucket="cholmes", Prefix="admin-boundaries/", MaxKeys=1)


def test_a_person_token_for_another_audience_is_refused():
    assert WRONG_AUD_TOKEN, "PERSON_PROXY_URL is set but CI_WRONG_AUDIENCE_TOKEN is not"
    resp = exchange(WRONG_AUD_TOKEN)
    assert resp.status_code == 400, resp.text[:300]
    assert "InvalidIdentityToken" in resp.text


def test_a_person_token_with_a_tampered_signature_is_refused():
    assert ID_TOKEN, "PERSON_PROXY_URL is set but CI_WRITE_ID_TOKEN is not"
    tampered = ID_TOKEN[:-1] + ("A" if ID_TOKEN[-1] != "A" else "B")
    resp = exchange(tampered)
    assert resp.status_code == 400, resp.text[:300]
