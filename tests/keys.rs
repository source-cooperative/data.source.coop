//! Native unit tests for the wasm-free half of the API-key exchange (`keys`),
//! included via `#[path]` like `tests/sts.rs`. The Cache API and the lookup
//! itself are wasm-only and are covered by `tests/test_api_keys.py`.

#[path = "../src/keys.rs"]
mod keys;
#[path = "../src/sts.rs"]
mod sts;

use keys::*;
use multistore_sts::TokenKey;

// Keys whose checksums were computed independently, with Python's zlib.crc32.
const KEY: &str = "sck_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1yLcDB";

// ── recognising a key ──────────────────────────────────────────────

#[test]
fn a_well_formed_key_is_a_key() {
    assert_eq!(parse_api_key(KEY), Some(KEY));
    // Every character class, and a CRC above 2^31.
    let mixed = "sck_0123456789ABCDEFGHIJabcdefghij4Us3aw";
    assert_eq!(parse_api_key(mixed), Some(mixed));
    // A CRC below 62^5, whose checksum keeps its leading zero.
    let padded = "sck_000000000000000000000000000001010Ohw";
    assert_eq!(parse_api_key(padded), Some(padded));
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
    assert_eq!(parse_api_key(&KEY[..39]), None, "too short");
    assert_eq!(parse_api_key(&format!("{KEY}a")), None, "too long");
    assert_eq!(
        parse_api_key(&KEY.replace("sck_", "SCK_")),
        None,
        "wrong case"
    );
    assert_eq!(
        parse_api_key(&KEY.replacen('a', "b", 1)),
        None,
        "a mistyped character"
    );
    assert_eq!(
        parse_api_key("sck_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1yLcDC"),
        None,
        "a mistyped checksum"
    );
    assert_eq!(
        parse_api_key("sck_aaaaaaaaaaaaaaaaaaaaaaaaaa-_aa1yLcDB"),
        None,
        "not base62"
    );
    assert_eq!(
        parse_api_key("sck_Ab-_09Ab-_09Ab-_09Ab-_09Ab-_09Ab-_09Ab-_09A"),
        None,
        "the checksum-less 47-character format"
    );
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
    // Computed independently: sha256 of KEY.
    assert_eq!(
        key_hash(KEY),
        "613aab548f220de88af7132782834dd7af6ec9ded8df4a7b19840545280968db"
    );
    assert_eq!(key_hash(KEY).len(), 64);
}

// ── minting ────────────────────────────────────────────────────────

fn token_key() -> TokenKey {
    TokenKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=").unwrap()
}

fn role(cap: u64) -> multistore::types::RoleConfig {
    sts::default_role("https://auth.example.test".into(), vec!["aud".into()], cap)
}

#[test]
fn credentials_are_sealed_for_the_account_within_floor_and_cap() {
    let key = token_key();
    let creds = credentials_for(&role(43_200), "acme--nightly-sync", None, &key).unwrap();
    assert_eq!(creds.source_identity, "acme--nightly-sync");
    assert_eq!(creds.assumed_role_id, "_default");
    assert!(creds.access_key_id.starts_with("STSPRXY"));
    // Sealed: the session token unseals to these credentials.
    let unsealed = key.unseal(&creds.session_token).unwrap().unwrap();
    assert_eq!(unsealed.source_identity, "acme--nightly-sync");

    let now = chrono_now();
    let default = credentials_for(&role(43_200), "a", None, &key).unwrap();
    assert!((default.expiration.timestamp() - now - 3600).abs() <= 2);
    let floored = credentials_for(&role(43_200), "a", Some(1), &key).unwrap();
    assert!((floored.expiration.timestamp() - now - 900).abs() <= 2);
    let capped = credentials_for(&role(3_600), "a", Some(86_400), &key).unwrap();
    assert!((capped.expiration.timestamp() - now - 3600).abs() <= 2);
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
