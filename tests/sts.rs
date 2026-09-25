//! Native unit tests for Role lookup in `sts`, included via `#[path]` (the lib
//! itself is `cdylib` with `test = false`). Mirrors the pattern in
//! `tests/backend_auth.rs`. What each Role's ceiling allows is pinned in
//! `tests/authz.rs`, next to the check that enforces it.

#[path = "../src/sts.rs"]
mod sts;

/// The Role `role_arn` resolves to, by id.
fn named(role_arn: &str) -> Option<String> {
    sts::role(
        role_arn,
        "https://auth.example.test".into(),
        vec!["aud".into()],
        3600,
    )
    .map(|role| role.role_id)
}

#[test]
fn each_role_is_served_by_its_bare_name() {
    for name in ["FullAccess", "ReadOnly", "_default"] {
        assert_eq!(named(name).as_deref(), Some(name));
    }
}

#[test]
fn arn_forms_are_accepted_for_any_partition_and_account() {
    for arn in [
        "arn:aws:iam::000000000000:role/ReadOnly",
        "arn:aws:iam::123456789012:role/ReadOnly",
        "arn:aws-us-gov:iam::123456789012:role/ReadOnly",
        // A service account's id, as source.coop's GitHub snippet names it.
        "arn:aws:iam::acme--nightly-sync:role/ReadOnly",
    ] {
        assert_eq!(named(arn).as_deref(), Some("ReadOnly"), "{arn}");
    }
    assert_eq!(
        named("arn:aws:iam::acme--nightly-sync:role/FullAccess").as_deref(),
        Some("FullAccess")
    );
    // Deployed client configuration uses this one.
    assert_eq!(
        named("arn:aws:iam::000000000000:role/_default").as_deref(),
        Some("_default")
    );
}

#[test]
fn unknown_names_are_refused_not_defaulted() {
    for arn in [
        "",
        "default",
        "readonly",
        "Admin",
        "arn:aws:iam::123456789012:role/other",
        // No arn: prefix.
        "role/_default",
        // A pathed resource is not the Role.
        "arn:aws:iam::123456789012:role/team/_default",
        // A Role's name as the suffix of another.
        "arn:aws:iam::123456789012:role/not_default",
        // Not a role resource.
        "arn:aws:iam::123456789012:user/ReadOnly",
    ] {
        assert_eq!(named(arn), None, "{arn}");
    }
}

#[test]
fn the_account_is_the_arns_account_segment() {
    assert_eq!(
        sts::account("arn:aws:iam::acme--nightly-sync:role/FullAccess"),
        Some("acme--nightly-sync")
    );
    assert_eq!(
        sts::account("arn:aws:iam::000000000000:role/_default"),
        Some("000000000000")
    );
    for role_arn in [
        "FullAccess",
        "arn:aws:iam:::role/FullAccess",
        "arn:aws:iam::acme",
    ] {
        assert_eq!(sts::account(role_arn), None, "{role_arn}");
    }
}
