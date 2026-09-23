//! Native unit tests for the wasm-free `keys` module, included via `#[path]`
//! (the lib itself is `cdylib` with `test = false`). Mirrors the pattern in
//! `tests/backend_auth.rs`.

#[path = "../src/keys.rs"]
mod keys;

use keys::*;
use multistore_oidc_provider::jwt::JwtSigner;
use multistore_sts::TokenKey;

const ISSUER: &str = "https://data.example.test";

fn signer(kid: &str) -> JwtSigner {
    use rsa::pkcs8::EncodePrivateKey;
    let key = rsa::RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048).unwrap();
    let pem = key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
    JwtSigner::from_pem(&pem, kid.into(), 60).unwrap()
}

fn request(expires_at: Option<&str>) -> KeyRequest {
    KeyRequest {
        account_id: "nightly-sync".into(),
        jti: "8b1c2d3e".into(),
        expires_at: expires_at.map(String::from),
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn rfc3339(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .unwrap()
        .to_rfc3339()
}

fn role() -> multistore::types::RoleConfig {
    api_key_role(ISSUER, 3600)
}

// ── minting ────────────────────────────────────────────────────────

#[test]
fn claims_carry_the_record_and_address_the_proxy() {
    let exp = now() + 86_400;
    let claims = api_key_claims(ISSUER, &request(Some(&rfc3339(exp))), now()).unwrap();
    assert_eq!(claims["iss"], ISSUER);
    assert_eq!(claims["aud"], ISSUER);
    assert_eq!(claims["sub"], "nightly-sync");
    assert_eq!(claims["jti"], "8b1c2d3e");
    assert_eq!(claims["type"], API_KEY_TYPE);
    assert_eq!(claims["exp"], exp);
}

#[test]
fn a_key_without_expiry_has_no_exp_claim() {
    let claims = api_key_claims(ISSUER, &request(None), now()).unwrap();
    assert!(claims.get("exp").is_none());
}

#[test]
fn minting_refuses_a_past_expiry_and_empty_fields() {
    assert!(api_key_claims(ISSUER, &request(Some(&rfc3339(now() - 1))), now()).is_err());
    assert!(api_key_claims(ISSUER, &request(Some("tomorrow")), now()).is_err());
    let mut blank = request(None);
    blank.jti.clear();
    assert!(api_key_claims(ISSUER, &blank, now()).is_err());
}

#[test]
fn only_prefixed_tokens_are_keys() {
    assert_eq!(strip_api_key("sck_a.b.c"), Some("a.b.c"));
    assert_eq!(strip_api_key("a.b.c"), None);
    assert_eq!(strip_api_key("SCK_a.b.c"), None);
}

// ── verifying ──────────────────────────────────────────────────────

#[test]
fn a_minted_key_verifies_to_its_record() {
    let s = signer("k1");
    let key = mint_api_key(&s, ISSUER, &request(None), now()).unwrap();
    assert!(key.starts_with("sck_eyJ"));
    let jwt = strip_api_key(&key).unwrap();
    let verified = verify_api_key(jwt, &own_jwks(&[&s]), ISSUER, &role()).unwrap();
    assert_eq!(verified.account_id, "nightly-sync");
    assert_eq!(verified.jti, "8b1c2d3e");
}

#[test]
fn an_expired_key_is_refused() {
    let s = signer("k1");
    let then = now() - 7_200;
    let key = mint_api_key(&s, ISSUER, &request(Some(&rfc3339(then + 60))), then).unwrap();
    let err = verify_api_key(
        strip_api_key(&key).unwrap(),
        &own_jwks(&[&s]),
        ISSUER,
        &role(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("expired"), "{err}");
}

#[test]
fn a_key_signed_before_a_rotation_verifies_while_the_old_key_is_served() {
    let previous = signer("k1");
    let current = signer("k2");
    let key = mint_api_key(&previous, ISSUER, &request(None), now()).unwrap();
    let jwt = strip_api_key(&key).unwrap();
    assert!(verify_api_key(jwt, &own_jwks(&[&current, &previous]), ISSUER, &role()).is_ok());
    assert!(verify_api_key(jwt, &own_jwks(&[&current]), ISSUER, &role()).is_err());
}

#[test]
fn a_federation_assertion_under_the_same_key_is_not_a_key() {
    let s = signer("k1");
    let assertion = s.sign("nightly-sync", ISSUER, ISSUER, &[]).unwrap();
    let err = verify_api_key(&assertion, &own_jwks(&[&s]), ISSUER, &role()).unwrap_err();
    assert!(err.to_string().contains("not an API key"), "{err}");
}

#[test]
fn a_key_for_another_issuer_is_refused() {
    let s = signer("k1");
    let key = mint_api_key(&s, "https://elsewhere.test", &request(None), now()).unwrap();
    assert!(verify_api_key(
        strip_api_key(&key).unwrap(),
        &own_jwks(&[&s]),
        ISSUER,
        &role()
    )
    .is_err());
}

// ── credentials ────────────────────────────────────────────────────

#[test]
fn credentials_are_sealed_for_the_account_within_the_cap() {
    let s = signer("k1");
    let key = mint_api_key(&s, ISSUER, &request(None), now()).unwrap();
    let verified = verify_api_key(
        strip_api_key(&key).unwrap(),
        &own_jwks(&[&s]),
        ISSUER,
        &role(),
    )
    .unwrap();
    let token_key = TokenKey::from_base64(&format!("{}=", "A".repeat(43))).unwrap();

    let creds = credentials_for(&role(), &verified, Some(10), &token_key).unwrap();
    let unsealed = token_key.unseal(&creds.session_token).unwrap().unwrap();
    assert_eq!(unsealed.source_identity, "nightly-sync");
    assert_eq!(unsealed.assumed_role_id, "_default");
    let lifetime = (creds.expiration - chrono::Utc::now()).num_seconds();
    assert!(
        (880..=900).contains(&lifetime),
        "floored at 900s, got {lifetime}"
    );
}
