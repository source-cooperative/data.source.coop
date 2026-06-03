//! TEMPORARY federation spike endpoint — `GET /_test`.
//!
//! NOT FOR PRODUCTION. This module exists to validate, end to end, that the
//! proxy's OIDC identity can be federated into an AWS IAM role:
//!
//!   1. mint a short-lived RS256 OIDC assertion with the proxy's signing key,
//!   2. exchange it at AWS STS via `AssumeRoleWithWebIdentity`,
//!   3. use the returned temporary credentials to `ListObjectsV2` a bucket,
//!
//! and return a step-by-step JSON trace so failures are easy to diagnose.
//!
//! ## One-time setup (you do this)
//!
//! 1. Deploy this branch to a Cloudflare preview and note its URL, e.g.
//!    `https://<alias>-source-data-proxy.<subdomain>.workers.dev`.
//! 2. Set `OIDC_PROVIDER_ISSUER` (wrangler var) to that exact preview URL so the
//!    minted token's `iss` and the served `/.well-known/openid-configuration`
//!    both report it. (AWS fetches `{iss}/.well-known/openid-configuration`.)
//! 3. In AWS: create an IAM OIDC identity provider for that same URL, audience
//!    `TEST_AUDIENCE` below. Create a role (`TEST_ROLE_ARN`) whose trust policy
//!    allows `AssumeRoleWithWebIdentity` with
//!    `StringEquals { "<host>:aud": "source-coop-data-proxy" }` (optionally a
//!    `<host>:sub` condition matching `TEST_SUBJECT`), and a permission policy
//!    granting `s3:ListBucket` on `arn:aws:s3:::<TEST_BUCKET>`.
//! 4. Fill in the constants below, redeploy, and hit `GET /_test`.
//!
//! Remove this module (and its wiring in `lib.rs`) before shipping anything real.

use crate::config::AppConfig;
use hmac::{Hmac, Mac};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC};
use serde_json::json;
use sha2::{Digest, Sha256};

// ── Hardcoded spike configuration — EDIT THESE ──────────────────────────────

/// IAM role to assume. Its trust policy must allow the proxy's OIDC issuer.
const TEST_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/source-coop-federation-spike";
/// Bucket to list with the assumed-role credentials.
const TEST_BUCKET: &str = "alukach-demo-bucket";
/// Region of the bucket / the STS endpoint to call.
const TEST_REGION: &str = "us-west-2";
/// Audience claim in the minted token. Must match the role's trust-policy `aud`.
const TEST_AUDIENCE: &str = "source-coop-data-proxy";
/// Subject claim. Optionally matched by a `sub` condition in the trust policy.
const TEST_SUBJECT: &str = "scv1:conn:test:federation-spike";
/// Optional key prefix to list (empty = whole bucket).
const TEST_PREFIX: &str = "";

// ── Endpoint handler ────────────────────────────────────────────────────────

/// Handle `GET /_test`. Always returns 200 with a JSON trace describing each
/// step (so a failure at any stage is visible in the body rather than as an
/// opaque error).
pub async fn handle(config: &AppConfig) -> web_sys::Response {
    let issuer = config.oidc.issuer.clone();

    // Step 1 — mint the OIDC assertion.
    let token = match config
        .oidc
        .signer
        .sign(TEST_SUBJECT, &issuer, TEST_AUDIENCE, &[])
    {
        Ok(t) => t,
        Err(e) => {
            return json_response(
                500,
                &json!({
                    "step": "mint_token",
                    "ok": false,
                    "error": e.to_string(),
                })
                .to_string(),
            )
        }
    };

    let mut trace = json!({
        "config": {
            "issuer_used": issuer,
            "discovery_url": format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/')),
            "audience": TEST_AUDIENCE,
            "subject": TEST_SUBJECT,
            "role_arn": TEST_ROLE_ARN,
            "bucket": TEST_BUCKET,
            "region": TEST_REGION,
        },
        "mint_token": { "ok": true },
    });

    // Step 2 — exchange the assertion for temporary credentials.
    let creds = match assume_role(&token).await {
        Ok(c) => {
            trace["sts"] = json!({
                "ok": true,
                "access_key_id": c.access_key_id,
                "expiration": c.expiration,
            });
            c
        }
        Err(e) => {
            trace["sts"] = json!({ "ok": false, "error": e });
            return json_response(200, &trace.to_string());
        }
    };

    // Step 3 — list the bucket with the assumed-role credentials.
    match list_bucket(&creds).await {
        Ok(list) => {
            trace["s3_list"] = json!({ "ok": true, "status": list.status, "keys": list.keys, "raw": clip(&list.body, 4000) })
        }
        Err(e) => trace["s3_list"] = json!({ "ok": false, "error": e }),
    }

    json_response(200, &trace.to_string())
}

// ── STS AssumeRoleWithWebIdentity ───────────────────────────────────────────

struct TempCreds {
    access_key_id: String,
    secret_access_key: String,
    session_token: String,
    expiration: String,
}

async fn assume_role(web_identity_token: &str) -> Result<TempCreds, String> {
    let url = format!("https://sts.{}.amazonaws.com/", TEST_REGION);
    let body = form_encode(&[
        ("Action", "AssumeRoleWithWebIdentity"),
        ("Version", "2011-06-15"),
        ("DurationSeconds", "900"),
        ("RoleArn", TEST_ROLE_ARN),
        ("RoleSessionName", "federation-spike"),
        ("WebIdentityToken", web_identity_token),
    ]);

    let resp = crate::http_client()
        .post(&url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("STS request failed: {e}"))?;

    let status = resp.status().as_u16();
    let xml = resp
        .text()
        .await
        .map_err(|e| format!("reading STS body failed: {e}"))?;

    if status != 200 {
        // AWS error XML is the most useful debug signal (e.g. audience/sub
        // mismatch, issuer not trusted) — surface it verbatim.
        return Err(format!("STS returned {status}: {}", clip(&xml, 2000)));
    }

    let get = |tag: &str| xml_tag(&xml, tag).map(str::to_string);
    Ok(TempCreds {
        access_key_id: get("AccessKeyId").ok_or("STS response missing AccessKeyId")?,
        secret_access_key: get("SecretAccessKey").ok_or("STS response missing SecretAccessKey")?,
        session_token: get("SessionToken").ok_or("STS response missing SessionToken")?,
        expiration: get("Expiration").unwrap_or_default(),
    })
}

// ── S3 ListObjectsV2 (SigV4-signed) ─────────────────────────────────────────

struct ListResult {
    status: u16,
    keys: Vec<String>,
    body: String,
}

async fn list_bucket(creds: &TempCreds) -> Result<ListResult, String> {
    let host = format!("{}.s3.{}.amazonaws.com", TEST_BUCKET, TEST_REGION);

    // Canonical query string: sorted, AWS-percent-encoded.
    let mut params: Vec<(String, String)> = vec![
        ("list-type".into(), "2".into()),
        ("max-keys".into(), "100".into()),
    ];
    if !TEST_PREFIX.is_empty() {
        params.push(("prefix".into(), TEST_PREFIX.into()));
    }
    params.sort();
    let canonical_query = params
        .iter()
        .map(|(k, v)| format!("{}={}", aws_encode(k), aws_encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    let (amz_date, datestamp) = amz_times();
    let payload_hash = sha256_hex(b"");

    let canonical_headers = format!(
        "host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\nx-amz-security-token:{token}\n",
        token = creds.session_token,
    );
    let signed_headers = "host;x-amz-content-sha256;x-amz-date;x-amz-security-token";
    let canonical_request =
        format!("GET\n/\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}");

    let scope = format!("{datestamp}/{}/s3/aws4_request", TEST_REGION);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    // Derive the signing key and sign.
    let k_date = hmac(
        format!("AWS4{}", creds.secret_access_key).as_bytes(),
        datestamp.as_bytes(),
    );
    let k_region = hmac(&k_date, TEST_REGION.as_bytes());
    let k_service = hmac(&k_region, b"s3");
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex::encode(hmac(&k_signing, string_to_sign.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        creds.access_key_id,
    );

    let url = format!("https://{host}/?{canonical_query}");
    let resp = crate::http_client()
        .get(&url)
        .header("x-amz-date", amz_date.as_str())
        .header("x-amz-content-sha256", payload_hash.as_str())
        .header("x-amz-security-token", creds.session_token.as_str())
        .header("authorization", authorization.as_str())
        .send()
        .await
        .map_err(|e| format!("S3 request failed: {e}"))?;

    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("reading S3 body failed: {e}"))?;

    let keys = if status == 200 {
        xml_tags(&body, "Key")
    } else {
        Vec::new()
    };
    Ok(ListResult { status, keys, body })
}

// ── SigV4 / crypto helpers ──────────────────────────────────────────────────

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// AWS canonical URI/query encoding: everything except `A-Za-z0-9-_.~`.
const AWS_UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

fn aws_encode(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, AWS_UNRESERVED).to_string()
}

/// `application/x-www-form-urlencoded` body (percent-encoded; no values here
/// contain spaces, so `+` handling is unnecessary).
fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", aws_encode(k), aws_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Current time as (`YYYYMMDDTHHMMSSZ`, `YYYYMMDD`), derived from the JS clock so
/// no chrono wasm-clock feature is needed.
fn amz_times() -> (String, String) {
    let epoch_secs = (js_sys::Date::now() / 1000.0) as i64;
    let days = epoch_secs.div_euclid(86400);
    let sod = epoch_secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    (
        format!("{y:04}{m:02}{d:02}T{hh:02}{mm:02}{ss:02}Z"),
        format!("{y:04}{m:02}{d:02}"),
    )
}

/// Howard Hinnant's days-from-civil, inverted: epoch-day -> (year, month, day).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ── tiny XML + response helpers ─────────────────────────────────────────────

/// Extract the text of the first `<tag>...</tag>`. STS/S3 responses are simple
/// enough that a substring scan beats pulling in a full XML parser here.
fn xml_tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim())
}

/// Extract the text of every `<tag>...</tag>` occurrence.
fn xml_tags(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(s) = rest.find(&open) {
        let after = &rest[s + open.len()..];
        let Some(e) = after.find(&close) else { break };
        out.push(after[..e].trim().to_string());
        rest = &after[e + close.len()..];
    }
    out
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}… [{} bytes total]", &s[..max], s.len())
    }
}

fn json_response(status: u16, body: &str) -> web_sys::Response {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    let resp = web_sys::Response::new_with_opt_str_and_init(Some(body), &init)
        .unwrap_or_else(|_| web_sys::Response::new().unwrap());
    let _ = resp.headers().set("content-type", "application/json");
    resp
}
