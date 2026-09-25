//! Native unit tests for the wasm-free half of platform-IdP exchanges
//! (`platform`), included via `#[path]` like `tests/keys.rs`. Verification
//! against a real issuer's keys and the trust lookup run in the worker and are
//! covered by `tests/test_platform_trust.py`.

#[path = "../src/platform.rs"]
mod platform;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::json;

const GITHUB: &str = "https://token.actions.githubusercontent.com";

// ── configuration ──────────────────────────────────────────────────

#[test]
fn each_issuer_keeps_its_own_audiences() {
    let issuers = platform::parse_issuers(
        r#"{"https://token.actions.githubusercontent.com": ["https://data.source.coop"],
            "https://gitlab.com": ["a", "b"]}"#,
    );
    assert_eq!(issuers[GITHUB], ["https://data.source.coop"]);
    assert_eq!(issuers["https://gitlab.com"], ["a", "b"]);
}

#[test]
fn an_issuer_without_an_audience_is_not_trusted() {
    let issuers = platform::parse_issuers(r#"{"https://token.actions.githubusercontent.com": []}"#);
    assert!(issuers.is_empty());
}

#[test]
fn a_value_that_does_not_parse_trusts_no_issuer() {
    for value in [
        "",
        GITHUB,
        r#"["https://token.actions.githubusercontent.com"]"#,
    ] {
        assert!(platform::parse_issuers(value).is_empty(), "{value}");
    }
}

// ── reading a token before verifying it ────────────────────────────

fn segment(value: serde_json::Value) -> String {
    URL_SAFE_NO_PAD.encode(value.to_string())
}

#[test]
fn a_jwt_is_read_without_verifying_it() {
    let token = format!(
        "{}.{}.not-a-signature",
        segment(json!({"alg": "RS256", "kid": "k1"})),
        segment(json!({"iss": GITHUB, "sub": "repo:o/r:ref:refs/heads/main"})),
    );
    let (header, claims) = platform::unverified(&token).unwrap();
    assert_eq!(header["kid"], "k1");
    assert_eq!(claims["iss"], GITHUB);
}

#[test]
fn anything_else_is_not_read() {
    let claims_not_json = format!("{}.not-json.sig", segment(json!({"alg": "RS256"})));
    for token in [
        "",
        "not-a-jwt",
        "sck_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        claims_not_json.as_str(),
    ] {
        assert!(platform::unverified(token).is_none(), "{token}");
    }
}

// ── claims a platform token must carry ─────────────────────────────

#[test]
fn a_verified_token_names_its_subject() {
    let claims = json!({"sub": "repo:o/r:ref:refs/heads/main", "exp": 1_900_000_000});
    assert_eq!(
        platform::subject(&claims).unwrap(),
        "repo:o/r:ref:refs/heads/main"
    );
}

#[test]
fn a_token_without_an_expiry_or_a_subject_is_refused() {
    for claims in [
        json!({"sub": "repo:o/r:ref:refs/heads/main"}),
        json!({"exp": 1_900_000_000}),
        json!({"sub": "", "exp": 1_900_000_000}),
    ] {
        assert!(platform::subject(&claims).is_err(), "{claims}");
    }
}
