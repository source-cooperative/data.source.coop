//! Unit tests for the routing module.
//!
//! The main crate is a cdylib targeting wasm32-unknown-unknown, and its wasm-only
//! deps (worker, wasm-bindgen-futures, web-sys) won't compile on native targets.
//! Since `cargo test` runs natively, we can't test through the lib. Instead we
//! include `routing.rs` directly via `#[path]` — it's pure Rust with no wasm
//! deps, so it compiles and runs on the native target without issues.

#[path = "../src/routing.rs"]
mod routing;

use http::Method;
use routing::{parse_request, ParsedRequest};

#[test]
fn root_returns_index() {
    let result = parse_request(&Method::GET, "/", None);
    assert!(matches!(result, ParsedRequest::Index));
}

#[test]
fn trailing_slash_parses_same_as_without() {
    let with_slash = parse_request(
        &Method::GET,
        "/cholmes/",
        Some("list-type=2&prefix=admin-boundaries/"),
    );
    let without_slash = parse_request(
        &Method::GET,
        "/cholmes",
        Some("list-type=2&prefix=admin-boundaries/"),
    );
    match (&with_slash, &without_slash) {
        (
            ParsedRequest::ProductList {
                rewritten_path: p1,
                query: q1,
                prefix_route: Some(pr1),
            },
            ParsedRequest::ProductList {
                rewritten_path: p2,
                query: q2,
                prefix_route: Some(pr2),
            },
        ) => {
            assert_eq!(p1, p2);
            assert_eq!(q1, q2);
            assert_eq!(pr1.account, pr2.account);
            assert_eq!(pr1.product, pr2.product);
            assert_eq!(pr1.original_prefix, pr2.original_prefix);
        }
        _ => panic!("Expected ProductList for both, got different variants"),
    }
}

#[test]
fn url_encoded_prefix_decoded() {
    let result = parse_request(
        &Method::GET,
        "/cholmes",
        Some("list-type=2&prefix=admin-boundaries%2Fsubdir%2F"),
    );
    match result {
        ParsedRequest::ProductList {
            rewritten_path,
            query,
            prefix_route: Some(pr),
        } => {
            assert_eq!(rewritten_path, "/cholmes--admin-boundaries");
            assert_eq!(pr.product, "admin-boundaries");
            assert_eq!(pr.original_prefix, "admin-boundaries/subdir/");
            assert!(query.contains("prefix=subdir/"));
        }
        _ => panic!("Expected ProductList with prefix_route"),
    }
}

#[test]
fn object_request_basic() {
    let result =
        parse_request(&Method::GET, "/cholmes/admin-boundaries/countries.parquet", None);
    match result {
        ParsedRequest::ObjectRequest {
            rewritten_path,
            query,
        } => {
            assert_eq!(
                rewritten_path,
                "/cholmes--admin-boundaries/countries.parquet"
            );
            assert!(query.is_none());
        }
        _ => panic!("Expected ObjectRequest"),
    }
}

#[test]
fn list_via_prefix() {
    let result = parse_request(
        &Method::GET,
        "/cholmes",
        Some("list-type=2&prefix=admin-boundaries/subdir/"),
    );
    match result {
        ParsedRequest::ProductList {
            rewritten_path,
            query,
            prefix_route: Some(pr),
        } => {
            assert_eq!(rewritten_path, "/cholmes--admin-boundaries");
            assert!(query.contains("prefix=subdir/"));
            assert_eq!(pr.account, "cholmes");
            assert_eq!(pr.product, "admin-boundaries");
            assert_eq!(pr.original_prefix, "admin-boundaries/subdir/");
        }
        _ => panic!("Expected ProductList with prefix_route"),
    }
}

#[test]
fn list_via_segment() {
    let result = parse_request(
        &Method::GET,
        "/cholmes/admin-boundaries",
        Some("list-type=2"),
    );
    match result {
        ParsedRequest::ProductList {
            rewritten_path,
            query,
            prefix_route,
        } => {
            assert_eq!(rewritten_path, "/cholmes--admin-boundaries");
            assert_eq!(query, "list-type=2");
            assert!(prefix_route.is_none());
        }
        _ => panic!("Expected ProductList without prefix_route"),
    }
}

#[test]
fn account_list_no_prefix() {
    let result = parse_request(&Method::GET, "/cholmes", Some("list-type=2&delimiter=/"));
    match result {
        ParsedRequest::AccountList { account, .. } => {
            assert_eq!(account, "cholmes");
        }
        _ => panic!("Expected AccountList"),
    }
}

#[test]
fn write_methods_rejected() {
    for method in &[Method::PUT, Method::POST, Method::DELETE, Method::PATCH] {
        let result = parse_request(method, "/cholmes/admin-boundaries/file.txt", None);
        assert!(
            matches!(result, ParsedRequest::WriteNotAllowed),
            "{method} should be rejected"
        );
    }
}
