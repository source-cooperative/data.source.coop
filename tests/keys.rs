//! Native unit tests for the wasm-free half of the API-key exchange (`keys`),
//! included via `#[path]` like `tests/sts.rs`. The Cache API and the lookup
//! itself are wasm-only and are covered by `tests/test_api_keys.py`.

#[path = "../src/keys.rs"]
mod keys;
#[path = "../src/sts.rs"]
mod sts;

use keys::*;
use multistore_sts::TokenKey;

const KEY: &str = "sck_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

// ── recognising a key ──────────────────────────────────────────────

#[test]
fn a_well_formed_key_is_a_key() {
    assert_eq!(parse_api_key(KEY), Some(KEY));
    let mixed = "sck_Ab-_09Ab-_09Ab-_09Ab-_09Ab-_09Ab-_09Ab-_09A";
    assert_eq!(mixed.len(), 47);
    assert_eq!(parse_api_key(mixed), Some(mixed));
}

#[test]
fn surrounding_whitespace_is_trimmed() {
    // Every hand-made token file ends in a newline; some SDKs send it.
    assert_eq!(parse_api_key(&format!("{KEY}\n")), Some(KEY));
    assert_eq!(parse_api_key(&format!("{KEY}\r\n")), Some(KEY));
    assert_eq!(parse_api_key(&format!("  {KEY}  ")), Some(KEY));
}

#[test]
fn anything_else_is_not_a_key() {
    assert_eq!(parse_api_key(&KEY[..46]), None, "too short");
    assert_eq!(parse_api_key(&format!("{KEY}a")), None, "too long");
    assert_eq!(
        parse_api_key(&KEY.replace("sck_", "SCK_")),
        None,
        "wrong case"
    );
    assert_eq!(parse_api_key(&KEY.replace('a', "+")), None, "not base64url");
    assert_eq!(
        parse_api_key("eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ4In0.sig"),
        None,
        "a JWT"
    );
    assert_eq!(parse_api_key(""), None);
}

#[test]
fn the_prefix_alone_marks_a_key_for_refusal() {
    assert!(looks_like_api_key("sck_anything"));
    assert!(looks_like_api_key("  sck_anything"));
    assert!(!looks_like_api_key("eyJ.a.b"));
    assert!(!looks_like_api_key("SCK_anything"));
}

// ── hashing ────────────────────────────────────────────────────────

#[test]
fn the_hash_is_hex_sha256_of_the_key() {
    // Computed independently: sha256("sck_" + "a" * 43).
    assert_eq!(
        key_hash(KEY),
        "079124300599a6ace561d0554a60dccf90edb04d486b55062bba42ff230d4f5f"
    );
    assert_eq!(key_hash(KEY).len(), 64);
}

// ── minting ────────────────────────────────────────────────────────

fn token_key() -> TokenKey {
    TokenKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=").unwrap()
}

fn role(name: &str, cap: u64) -> multistore::types::RoleConfig {
    sts::role(
        name,
        "https://auth.example.test".into(),
        vec!["aud".into()],
        cap,
    )
    .unwrap()
}

#[test]
fn credentials_are_sealed_for_the_account_within_floor_and_cap() {
    let key = token_key();
    let creds =
        credentials_for(&role("_default", 43_200), "acme--nightly-sync", None, &key).unwrap();
    assert_eq!(creds.source_identity, "acme--nightly-sync");
    assert_eq!(creds.assumed_role_id, "_default");
    assert!(creds.access_key_id.starts_with("STSPRXY"));
    // Sealed: the session token unseals to these credentials.
    let unsealed = key.unseal(&creds.session_token).unwrap().unwrap();
    assert_eq!(unsealed.source_identity, "acme--nightly-sync");

    let now = chrono_now();
    let default = credentials_for(&role("_default", 43_200), "a", None, &key).unwrap();
    assert!((default.expiration.timestamp() - now - 3600).abs() <= 2);
    let floored = credentials_for(&role("_default", 43_200), "a", Some(1), &key).unwrap();
    assert!((floored.expiration.timestamp() - now - 900).abs() <= 2);
    let capped = credentials_for(&role("_default", 3_600), "a", Some(86_400), &key).unwrap();
    assert!((capped.expiration.timestamp() - now - 3600).abs() <= 2);
}

#[test]
fn credentials_carry_the_named_roles_ceiling() {
    let key = token_key();
    let read_only = role("ReadOnly", 3_600);
    let creds = credentials_for(&read_only, "acme--nightly-sync", None, &key).unwrap();
    let unsealed = key.unseal(&creds.session_token).unwrap().unwrap();
    assert_eq!(unsealed.assumed_role_id, "ReadOnly");
    assert_eq!(
        serde_json::to_value(&unsealed.allowed_scopes).unwrap(),
        serde_json::to_value(&read_only.allowed_scopes).unwrap()
    );
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
