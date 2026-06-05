//! Custom `BucketRegistry` that resolves product backends via source.coop API.

use multistore::api::response::BucketEntry;
use multistore::error::ProxyError;
use multistore::registry::{BucketRegistry, ResolvedBucket};
use multistore::types::{BucketConfig, ResolvedIdentity, S3Operation};
use serde::Deserialize;
use std::collections::HashMap;

/// Registry that resolves Source Cooperative products to multistore `BucketConfig`s
/// by calling the Source Cooperative API.
#[derive(Clone)]
pub struct SourceCoopRegistry {
    api_base_url: String,
    api_auth: crate::ApiAuth,
    pub(crate) request_id: String,
}

impl SourceCoopRegistry {
    pub fn new(api_base_url: String, api_auth: crate::ApiAuth, request_id: String) -> Self {
        Self {
            api_base_url,
            api_auth,
            request_id,
        }
    }

    /// Parse "account:product" bucket name into (account, product).
    fn parse_bucket_name(name: &str) -> Option<(&str, &str)> {
        name.split_once(crate::BUCKET_SEPARATOR)
    }

    /// List products for an account via the Source API.
    pub async fn list_products(&self, account: &str) -> Result<Vec<String>, ProxyError> {
        let product_list = crate::cache::get_or_fetch_product_list(
            &self.api_base_url,
            account,
            &self.api_auth,
            &self.request_id,
            None,
        )
        .await?;
        Ok(product_list
            .products
            .into_iter()
            .map(|p| p.product_id)
            .collect())
    }
}

impl BucketRegistry for SourceCoopRegistry {
    async fn get_bucket(
        &self,
        name: &str,
        identity: &ResolvedIdentity,
        _operation: &S3Operation,
    ) -> Result<ResolvedBucket, ProxyError> {
        let (account, product) = Self::parse_bucket_name(name)
            .ok_or_else(|| ProxyError::BucketNotFound(name.to_string()))?;

        let subject = match identity {
            ResolvedIdentity::Authenticated(auth) => Some(auth.principal_name.as_str()),
            ResolvedIdentity::Anonymous => None,
        };

        let config = resolve_product(
            &self.api_base_url,
            account,
            product,
            &self.api_auth,
            &self.request_id,
            subject,
        )
        .await?;

        Ok(ResolvedBucket {
            config,
            list_rewrite: None,
            display_name: None,
        })
    }

    async fn list_buckets(
        &self,
        _identity: &ResolvedIdentity,
    ) -> Result<Vec<BucketEntry>, ProxyError> {
        unimplemented!("Bucket listing is not supported")
    }
}

/// Resolve a product to a `BucketConfig` by querying the Source Cooperative API.
async fn resolve_product(
    api_base_url: &str,
    account: &str,
    product: &str,
    api_auth: &crate::ApiAuth,
    request_id: &str,
    subject: Option<&str>,
) -> Result<BucketConfig, ProxyError> {
    let span = tracing::info_span!(
        "resolve_product",
        account = %account,
        product = %product,
        backend_type = tracing::field::Empty,
    );
    let _guard = span.enter();

    // 1. Fetch product metadata
    let source_product = crate::cache::get_or_fetch_product(
        api_base_url,
        account,
        product,
        api_auth,
        request_id,
        subject,
    )
    .await?;

    // 2. Find primary mirror
    let primary_key = &source_product.metadata.primary_mirror;
    let mirror = source_product
        .metadata
        .mirrors
        .get(primary_key)
        .or_else(|| source_product.metadata.mirrors.values().next())
        .ok_or_else(|| {
            ProxyError::BucketNotFound(format!("no mirrors for {}/{}", account, product))
        })?;

    // 3. Fetch data connections to resolve the actual bucket
    let connections =
        crate::cache::get_or_fetch_data_connections(api_base_url, api_auth, request_id, subject)
            .await?;

    let connection = connections
        .iter()
        .find(|c| c.data_connection_id == mirror.connection_id)
        .ok_or_else(|| {
            ProxyError::Internal(format!(
                "data connection '{}' not found",
                mirror.connection_id
            ))
        })?;

    // 4. Build BucketConfig
    let backend_type = match connection.details.provider.as_str() {
        "s3" => "s3",
        "az" | "azure" => "az",
        "gcs" | "gs" => "gcs",
        other => {
            return Err(ProxyError::Internal(format!(
                "unsupported provider: {}",
                other
            )))
        }
    }
    .to_string();

    let mut backend_options = HashMap::new();

    match backend_type.as_str() {
        "s3" => {
            if let Some(ref bucket) = connection.details.bucket {
                backend_options.insert("bucket_name".to_string(), bucket.clone());
            }
            if let Some(ref region) = connection.details.region {
                backend_options.insert("region".to_string(), region.clone());
                backend_options.insert(
                    "endpoint".to_string(),
                    format!("https://s3.{}.amazonaws.com", region),
                );
            }
        }
        "az" => {
            if let Some(ref account_name) = connection.details.account_name {
                backend_options.insert("account_name".to_string(), account_name.clone());
            }
            if let Some(ref container) = connection.details.container_name {
                backend_options.insert("container_name".to_string(), container.clone());
            }
        }
        _ => {}
    }

    // Backend authentication: unsigned (public) by default, or federate the
    // proxy's OIDC identity into the connection's role. Connections omit
    // `authentication` until a role is configured, so this stays unsigned today.
    apply_backend_auth(
        &connection.authentication,
        &connection.data_connection_id,
        &mut backend_options,
    );

    // 5. Build prefix: connection.base_prefix + mirror.prefix
    let base_prefix = connection.details.base_prefix.as_deref().unwrap_or("");
    let mirror_prefix = &mirror.prefix;
    let full_prefix = format!("{}{}", base_prefix, mirror_prefix);
    let backend_prefix = if full_prefix.is_empty() {
        None
    } else {
        Some(full_prefix)
    };

    let config = BucketConfig {
        name: format!("{}{}{}", account, crate::BUCKET_SEPARATOR, product),
        backend_type,
        backend_prefix,
        anonymous_access: true,
        allowed_roles: vec![],
        backend_options,
    };

    span.record("backend_type", config.backend_type.as_str());
    tracing::debug!(
        prefix = ?config.backend_prefix,
        options = ?config.backend_options,
        "product resolved",
    );

    Ok(config)
}

/// `aud` claim for AWS `AssumeRoleWithWebIdentity` assertions. This is AWS's
/// web-identity convention — the value the customer registers their IAM OIDC
/// provider with and conditions the role trust policy on — so it is constant
/// across connections rather than per-connection config.
const AWS_STS_AUDIENCE: &str = "sts.amazonaws.com";

/// Translate a connection's [`BackendAuth`] into multistore `backend_options`.
///
/// - [`Unsigned`](BackendAuth::Unsigned) sets `skip_signature` so the proxy
///   issues an unsigned request to a public bucket.
/// - [`S3WebIdentityRole`](BackendAuth::S3WebIdentityRole) hands the role ARN, a
///   per-connection subject (`scv1:conn:{id}`), and the fixed AWS audience to
///   multistore's OIDC backend-auth middleware, which mints the assertion,
///   exchanges it at AWS STS, and injects the temporary credentials — clearing
///   `skip_signature` so the request is signed.
/// - [`Unsupported`](BackendAuth::Unsupported) can't be fulfilled, so it serves
///   unsigned (a private backend then rejects the request).
///
/// NOTE: the federated path only takes effect once the `MaybeOidcAuth` middleware
/// is wired into dispatch (next step); multistore currently also takes the
/// audience at provider construction. Until both land the federated branches are
/// inert, but the option set is what the middleware will consume.
fn apply_backend_auth(
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
            // The audience is the fixed AWS web-identity value for every
            // connection (recorded per bucket so each cloud can carry its own
            // once multi-cloud federation lands).
            options.insert("oidc_audience".to_string(), AWS_STS_AUDIENCE.to_string());
        }
        // Unknown/unsupported auth type — e.g. the app-side `gcp_workload_identity`
        // / `azure_workload_identity` variants, which have no proxy/multistore
        // support yet. Don't sign with a scheme we can't fulfill: serve unsigned so
        // public buckets still work; a private backend rejects the unsigned request,
        // surfacing the misconfiguration instead of silently mis-signing.
        BackendAuth::Unsupported => {
            tracing::warn!(
                connection_id,
                "unsupported backend authentication type; serving unsigned"
            );
            options.insert("skip_signature".to_string(), "true".to_string());
        }
    }
}

// ── API response types ─────────────────────────────────────────────

/// Product visibility, mirroring `ProductVisibility` in the source.coop data
/// model. Replaced the legacy `data_mode` field in source.coop#284. Any missing
/// or unrecognized value deserializes to `Unknown`, which is treated as
/// non-public so we fail closed.
#[derive(Debug, Default, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    Unlisted,
    Restricted,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceProduct {
    pub product_id: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub visibility: Visibility,
    pub metadata: SourceProductMetadata,
}

impl SourceProduct {
    pub fn is_public(&self) -> bool {
        !self.disabled && self.visibility == Visibility::Public
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceProductMetadata {
    pub mirrors: HashMap<String, SourceProductMirror>,
    pub primary_mirror: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct SourceProductMirror {
    pub storage_type: String,
    pub connection_id: String,
    pub prefix: String,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DataConnection {
    pub data_connection_id: String,
    pub details: DataConnectionDetails,
    /// How the proxy authenticates to this connection's backend. A sibling of
    /// `details`, matching the Source API's `DataConnection` shape. Absent in the
    /// API response → [`BackendAuth::Unsigned`] (public bucket), preserving the
    /// pre-federation behavior for every connection that hasn't opted in.
    #[serde(default)]
    pub authentication: BackendAuth,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DataConnectionDetails {
    pub provider: String,
    pub bucket: Option<String>,
    pub region: Option<String>,
    pub base_prefix: Option<String>,
    pub account_name: Option<String>,
    pub container_name: Option<String>,
}

/// Per-connection backend authentication, as reported by the Source API
/// (a sibling of `details` on the connection).
///
/// Internally tagged on `type`; defaults to [`Unsigned`](BackendAuth::Unsigned)
/// when the field is omitted, so existing connections keep issuing unsigned
/// requests until a role is configured for them. Unknown `type`s (e.g. the
/// app-side GCP/Azure workload-identity variants) deserialize to
/// [`Unsupported`](BackendAuth::Unsupported) instead of failing the request.
///
/// The AWS variant carries only `role_arn`; the audience is a fixed constant
/// ([`AWS_STS_AUDIENCE`]) applied in [`apply_backend_auth`], and session duration
/// / subject scope may be added later.
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

#[derive(Debug, Clone, Deserialize)]
pub struct SourceProductList {
    pub products: Vec<SourceProduct>,
}
