//! Per-connection backend authentication: the model the Source API reports for a
//! data connection, and its translation into multistore `backend_options`.
//!
//! Kept in its own module — free of wasm-only deps — so it can be unit-tested on
//! native targets despite the crate's `[lib] test = false`. See
//! `tests/backend_auth.rs`.

use serde::Deserialize;
use std::collections::HashMap;

/// `aud` claim for the proxy's AWS `AssumeRoleWithWebIdentity` assertions. AWS's
/// fixed web-identity convention — the value the customer registers their IAM
/// OIDC provider with and conditions the role trust policy on — so it is constant
/// across connections. Applied at the OIDC backend-auth provider (see `lib.rs`).
pub(crate) const AWS_STS_AUDIENCE: &str = "sts.amazonaws.com";

/// Per-connection backend authentication, as reported by the Source API
/// (a sibling of `details` on the connection).
///
/// Internally tagged on `type`; defaults to [`Unsigned`](BackendAuth::Unsigned)
/// when the field is omitted, so existing connections keep issuing unsigned
/// requests until a role is configured. Unknown `type`s (e.g. the app-side
/// GCP/Azure workload-identity variants) deserialize to
/// [`Unsupported`](BackendAuth::Unsupported) instead of failing the request.
///
/// The AWS variant carries only `role_arn`; the audience is the fixed constant
/// [`AWS_STS_AUDIENCE`] set on the OIDC backend-auth provider, and session
/// duration / subject scope may be added later.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackendAuth {
    /// Public bucket — issue unsigned requests, no backend credentials.
    #[default]
    Unsigned,
    /// Federate the proxy's OIDC identity into a customer-owned AWS role via
    /// `AssumeRoleWithWebIdentity`, signing backend requests with the resulting
    /// temporary credentials. (S3 only for now.)
    S3WebIdentityRole {
        /// ARN of the IAM role the proxy assumes for this connection.
        role_arn: String,
    },
    /// An authentication type this proxy build does not implement — e.g. the
    /// Source API's `gcp_workload_identity` / `azure_workload_identity` variants,
    /// scaffolded app-side but without proxy/multistore support yet. Captured via
    /// `#[serde(other)]` so an unknown `type` deserializes gracefully; treated as
    /// unsupported (served unsigned, with a warning).
    #[serde(other)]
    Unsupported,
}

/// Translate a connection's [`BackendAuth`] into multistore `backend_options`.
///
/// - [`Unsigned`](BackendAuth::Unsigned) sets `skip_signature` so the proxy
///   issues an unsigned request to a public bucket.
/// - [`S3WebIdentityRole`](BackendAuth::S3WebIdentityRole) hands the role ARN and
///   a per-connection subject (`scv1:conn:{id}`) to multistore's OIDC backend-auth
///   middleware (wired in `lib.rs`), which mints the assertion (with the fixed AWS
///   audience set on the provider), exchanges it at AWS STS, and injects the
///   temporary credentials — clearing `skip_signature` so the request is signed.
/// - [`Unsupported`](BackendAuth::Unsupported) can't be fulfilled, so it serves
///   unsigned (a private backend then rejects the request).
pub(crate) fn apply_backend_auth(
    auth: &BackendAuth,
    connection_id: &str,
    options: &mut HashMap<String, String>,
) {
    match auth {
        BackendAuth::Unsigned => {
            options.insert("skip_signature".to_string(), "true".to_string());
        }
        BackendAuth::S3WebIdentityRole { role_arn } => {
            options.insert("auth_type".to_string(), "oidc".to_string());
            options.insert("oidc_role_arn".to_string(), role_arn.clone());
            options.insert(
                "oidc_subject".to_string(),
                format!("scv1:conn:{connection_id}"),
            );
        }
        BackendAuth::Unsupported => {
            tracing::warn!(
                connection_id,
                "unsupported backend authentication type; serving unsigned"
            );
            options.insert("skip_signature".to_string(), "true".to_string());
        }
    }
}
