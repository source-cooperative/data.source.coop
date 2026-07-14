//! Deserialize every stub fixture (tests/fixtures/*.json, served by
//! tests/stub_api.py) through the proxy's real serde structs, asserting the
//! parsed *semantics* the integration tests depend on. This is what ties the
//! stub to src/source_api/types.rs: a field rename or a new required field
//! fails here at `cargo test` time instead of as a mystery 500 in the
//! integration job.
//!
//! Wasm-free modules are included via `#[path]` (the lib is `cdylib` with
//! `test = false`), mirroring tests/authz.rs.

#[path = "../src/backend_auth.rs"]
mod backend_auth;
#[path = "../src/source_api/types.rs"]
mod types;

use backend_auth::BackendAuth;
use types::{DataConnection, SourceProduct, SourceProductList};

#[test]
fn product_fixture_is_public_with_resolvable_mirror() {
    let p: SourceProduct = serde_json::from_str(include_str!("fixtures/product.json")).unwrap();
    assert!(
        p.is_public(),
        "reads through the stub assume a public product"
    );
    assert!(
        p.metadata.mirrors.contains_key(&p.metadata.primary_mirror),
        "primary_mirror must resolve or every request is BucketNotFound"
    );
}

#[test]
fn write_probe_product_fixture_is_public_with_resolvable_mirror() {
    let p: SourceProduct =
        serde_json::from_str(include_str!("fixtures/product_write_probe.json")).unwrap();
    assert!(p.is_public());
    assert!(p.metadata.mirrors.contains_key(&p.metadata.primary_mirror));
}

#[test]
fn read_connection_fixture_is_unsigned_and_writable() {
    let c: DataConnection =
        serde_json::from_str(include_str!("fixtures/data_connection.json")).unwrap();
    assert_eq!(
        c.authentication,
        BackendAuth::Unsigned,
        "CI reads the public bucket without credentials on purpose"
    );
    assert!(!c.read_only);
}

#[test]
fn write_probe_connection_fixture_is_federated() {
    let c: DataConnection =
        serde_json::from_str(include_str!("fixtures/data_connection_write_probe.json")).unwrap();
    assert!(
        matches!(c.authentication, BackendAuth::S3WebIdentityRole { .. }),
        "the write probe exists to route through the federated auth path"
    );
}

#[test]
fn restricted_product_fixture_is_not_public() {
    let p: SourceProduct =
        serde_json::from_str(include_str!("fixtures/product_restricted.json")).unwrap();
    assert!(
        !p.is_public(),
        "the restricted probe exists to exercise the non-public path"
    );
}

#[test]
fn product_list_wrapper_parses() {
    // The stub wraps the product fixture as {"products": [...]} for the
    // account listing route; pin that wrapper shape too.
    let json = format!(
        r#"{{"products":[{}]}}"#,
        include_str!("fixtures/product.json")
    );
    let l: SourceProductList = serde_json::from_str(&json).unwrap();
    assert_eq!(l.products.len(), 1);
}
