//! Native unit tests for the `_default` role alias in `sts`, included via
//! `#[path]` (the lib itself is `cdylib` with `test = false`). Mirrors the
//! pattern in `tests/backend_auth.rs`.

#[path = "../src/sts.rs"]
mod sts;

use sts::is_default_role;

#[test]
fn literal_default_accepted() {
    assert!(is_default_role("_default"));
}

#[test]
fn arn_alias_accepted_for_any_partition_and_account() {
    assert!(is_default_role("arn:aws:iam::000000000000:role/_default"));
    assert!(is_default_role("arn:aws:iam::123456789012:role/_default"));
    assert!(is_default_role(
        "arn:aws-us-gov:iam::123456789012:role/_default"
    ));
}

#[test]
fn other_roles_rejected() {
    assert!(!is_default_role(""));
    assert!(!is_default_role("_other"));
    assert!(!is_default_role("default"));
    assert!(!is_default_role("arn:aws:iam::123456789012:role/other"));
}

#[test]
fn alias_requires_arn_prefix_and_exact_resource() {
    // No arn: prefix.
    assert!(!is_default_role("role/_default"));
    // Pathed resource is not the `_default` role.
    assert!(!is_default_role(
        "arn:aws:iam::123456789012:role/team/_default"
    ));
    // `_default` as a suffix of another role name.
    assert!(!is_default_role(
        "arn:aws:iam::123456789012:role/not_default"
    ));
}
