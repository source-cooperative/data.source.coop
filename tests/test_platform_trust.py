"""Platform-IdP tokens at /.sts (ADR-014), against the stub Source API.

CI trusts GitHub Actions as a platform issuer (PLATFORM_ISSUERS in ci.yml). A
verified GitHub token acts as the account its RoleArn names, and only if the
stub's trusts route says that account trusts the token's issuer and subject:
TRUST_ACCOUNT trusts this repository's workflows, and no other account trusts
anything. The tests that need a real token run where CI mints one (see
test_writes.py); the others pin what is refused before any trust lookup.
"""

import base64
import json
import uuid
import xml.etree.ElementTree as ET

import pytest
import requests

from stub_api import TRUST_ACCOUNT, TRUSTED_ISSUER, WRITE_ACCOUNT
from test_writes import ID_TOKEN, PROXY_URL, needs_token

STUB_URL = "http://localhost:9000"
# The worker takes its request id from `cf-ray`, which `wrangler dev` does not
# set; the tests supply one.
RAY = "ci-ray-trust"


def exchange(token, role_arn):
    params = {"Action": "AssumeRoleWithWebIdentity", "RoleArn": role_arn, "WebIdentityToken": token}
    return requests.post(f"{PROXY_URL}/.sts", data=params, headers={"cf-ray": RAY})


def as_account(account):
    return f"arn:aws:iam::{account}:role/FullAccess"


def sts_fields(resp):
    return {el.tag.rpartition("}")[2]: el.text for el in ET.fromstring(resp.text).iter()}


def trust_lookups(account):
    return requests.get(f"{STUB_URL}/_stub/trust-exchange-counts").json().get(account, 0)


def forged():
    """A token that says it is GitHub's, signed by no one."""

    def segment(value):
        return base64.urlsafe_b64encode(json.dumps(value).encode()).rstrip(b"=").decode()

    claims = {
        "iss": TRUSTED_ISSUER,
        "sub": "repo:source-cooperative/data.source.coop:ref:refs/heads/main",
        "aud": "source-data-proxy-ci",
        "exp": 4_000_000_000,
    }
    return ".".join([segment({"alg": "RS256", "kid": "forged"}), segment(claims), segment("x")])


def test_a_platform_token_must_name_the_account_it_acts_as():
    resp = exchange(forged(), "arn:aws:iam:::role/FullAccess")
    assert resp.status_code == 400
    assert "RoleArn must name the account" in sts_fields(resp)["Message"]


def test_a_forged_token_is_refused_before_any_trust_lookup():
    before = trust_lookups(TRUST_ACCOUNT)
    resp = exchange(forged(), as_account(TRUST_ACCOUNT))
    assert resp.status_code == 400
    assert sts_fields(resp)["Code"] == "InvalidIdentityToken"
    assert trust_lookups(TRUST_ACCOUNT) == before


@needs_token
def test_a_trusted_workflow_gets_credentials_that_act_as_the_account():
    """The session's principal is the account, not the token's subject: the
    stub records who the proxy asked about a product as."""
    import boto3
    from botocore.config import Config
    from botocore.exceptions import ClientError

    resp = exchange(ID_TOKEN, as_account(TRUST_ACCOUNT))
    assert resp.status_code == 200, resp.text[:300]
    fields = sts_fields(resp)
    client = boto3.client(
        "s3",
        endpoint_url=PROXY_URL,
        aws_access_key_id=fields["AccessKeyId"],
        aws_secret_access_key=fields["SecretAccessKey"],
        aws_session_token=fields["SessionToken"],
        region_name="us-east-1",
        config=Config(s3={"addressing_style": "path"}),
    )
    # A product the stub has never heard of, so the lookup is not cached.
    product = f"principal-probe-{uuid.uuid4().hex}"
    with pytest.raises(ClientError):
        client.get_object(Bucket=WRITE_ACCOUNT, Key=f"{product}/x")
    subjects = requests.get(f"{STUB_URL}/_stub/product-lookup-subjects").json()
    assert subjects[f"/api/v1/products/{WRITE_ACCOUNT}/{product}"] == TRUST_ACCOUNT


@needs_token
def test_an_account_that_does_not_trust_the_workflow_refuses_it():
    resp = exchange(ID_TOKEN, as_account("ci-tests--someone-else"))
    assert resp.status_code == 403
    fields = sts_fields(resp)
    assert fields["Code"] == "AccessDenied"
    assert fields["Message"] == (
        f"Not authorized to perform sts:AssumeRoleWithWebIdentity (request id {RAY})"
    )


@needs_token
def test_a_trusted_answer_is_cached():
    exchange(ID_TOKEN, as_account(TRUST_ACCOUNT))
    before = trust_lookups(TRUST_ACCOUNT)
    assert exchange(ID_TOKEN, as_account(TRUST_ACCOUNT)).status_code == 200
    assert trust_lookups(TRUST_ACCOUNT) == before, "second exchange within the TTL asked the API again"
