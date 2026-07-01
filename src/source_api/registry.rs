//! Custom `BucketRegistry` that resolves product backends via source.coop API.

use multistore::api::response::BucketEntry;
use multistore::error::ProxyError;
use multistore::registry::{BucketRegistry, ResolvedBucket};
use multistore::types::{Action, BucketConfig, ResolvedIdentity, S3Operation};
use std::collections::HashMap;

use crate::authz::{decide_backend_auth, is_write_action};

use super::types::DataConnectionDetails;

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

    /// List products for an account via the Source API.
    pub async fn list_products(&self, account: &str) -> Result<Vec<String>, ProxyError> {
        let product_list = super::cache::get_or_fetch_product_list(
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
        operation: &S3Operation,
    ) -> Result<ResolvedBucket, ProxyError> {
        // Bucket names arrive pre-mapped as "account:product".
        let (account, product) = name
            .split_once(crate::BUCKET_SEPARATOR)
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
            is_write_action(operation.action()),
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

    async fn authorize_key(
        &self,
        _name: &str,
        _identity: &ResolvedIdentity,
        action: Action,
        _key: &str,
    ) -> bool {
        // Per-key authorization for batch delete. Correctness depends on
        // `get_bucket` having already authorized this caller for `name`: Source
        // Cooperative authorizes writes at the product level there (caller holds
        // product write permission, connection is writable), so every key in a
        // batch delete that reached this point is permitted. The multistore
        // default would deny every key, since callers' STS sessions carry no
        // per-bucket scopes. If a future multistore ever invoked `authorize_key`
        // without a prior successful `get_bucket` for the same `name`, this gate
        // alone would be insufficient — it only confirms the op is a write, not
        // that the caller is entitled. Gating on a write action is thus
        // defense-in-depth: only reached for write batch ops, never blanket-
        // allows a read.
        is_write_action(action)
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
    is_write: bool,
) -> Result<BucketConfig, ProxyError> {
    let span = tracing::info_span!(
        "resolve_product",
        account = %account,
        product = %product,
        backend_type = tracing::field::Empty,
        auth_type = tracing::field::Empty,
    );
    let _guard = span.enter();

    // 1. Fetch product metadata
    let source_product = super::cache::get_or_fetch_product(
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

    // 3. Fetch the referenced connection by id, so the subject-scoped API
    // authorizes this exact resource (404/403 → BucketNotFound/AccessDenied)
    // instead of the proxy resolving it out of an over-broad cached list.
    let connection = super::cache::get_or_fetch_data_connection(
        api_base_url,
        &mirror.connection_id,
        api_auth,
        request_id,
        subject,
    )
    .await?;

    // 4. Build BucketConfig
    let (backend_type, mut backend_options) = build_backend_options(&connection.details)?;

    // A write needs the caller's product permissions; fetch them only for an
    // authenticated write, since reads never consult them and an anonymous write
    // is denied before they're read. (Earlier revisions also skipped this fetch
    // for writes the connection itself could not accept — read-only / unsignable —
    // as a micro-opt. Consolidating the whole gate into `decide_backend_auth`
    // trades that for a single source of truth, at the cost of one extra API call
    // on that specific deny path.)
    let permissions = match subject {
        Some(subject) if is_write => {
            super::cache::get_or_fetch_permissions(
                api_base_url,
                account,
                product,
                api_auth,
                request_id,
                subject,
            )
            .await?
        }
        _ => Vec::new(),
    };

    // Backend authentication: unsigned (public) by default, or federate the
    // proxy's OIDC identity into the connection's role — but only after the write
    // gate passes. Both decisions live in `decide_backend_auth`.
    //
    // The confused-deputy guard is upstream: the subject-scoped Source API
    // fetches above (get_or_fetch_product / get_or_fetch_data_connection, keyed
    // on the caller's principal) only return the product/connection this caller is
    // authorized for — so reaching here means the caller is already cleared for
    // this connection's backend, hence we pass `Some(..)`. Federation does not
    // re-authorize.
    //
    // This ordering is enforced by data dependency, not just statement order:
    // `connection` only exists once the subject-scoped fetch succeeds, and a
    // 403/404 from that fetch propagates via `?` before we ever get here — so an
    // unauthorized caller can never reach federation. Guarded in CI by the
    // `tests/authz.rs` `decide_backend_auth` unit tests (an unauthorized outcome
    // ⇒ AccessDenied with no options emitted) and end-to-end by
    // tests/test_federation.py's test_restricted_product_denied_to_anonymous.
    span.record("auth_type", connection.authentication.kind());
    decide_backend_auth(
        Some(&connection.authentication),
        connection.read_only,
        is_write,
        subject.is_some(),
        &permissions,
        &connection.data_connection_id,
        &backend_type,
        &mut backend_options,
    )?;

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
        // Proxy-client-facing: Source Cooperative authorizes callers via its own
        // JWT (enforced upstream at the subject-scoped fetch), not S3 request
        // signing to the proxy — so the proxy accepts anonymous S3 requests and
        // does its own authz. Always true, including for private/federated
        // connections (whose backend auth is handled separately above).
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

/// Map a connection's raw `provider` to its multistore `backend_type`
/// (`s3`/`az`/`gcs`) and the provider-specific `backend_options`. Doing the
/// provider match once keeps the type and its options in a single source of
/// truth. (`gcs` carries no options yet.)
fn build_backend_options(
    details: &DataConnectionDetails,
) -> Result<(String, HashMap<String, String>), ProxyError> {
    let mut options = HashMap::new();
    let backend_type = match details.provider.as_str() {
        "s3" => {
            if let Some(ref bucket) = details.bucket {
                options.insert("bucket_name".to_string(), bucket.clone());
            }
            if let Some(ref region) = details.region {
                options.insert("region".to_string(), region.clone());
                options.insert(
                    "endpoint".to_string(),
                    format!("https://s3.{}.amazonaws.com", region),
                );
            }
            "s3"
        }
        "az" | "azure" => {
            if let Some(ref account_name) = details.account_name {
                options.insert("account_name".to_string(), account_name.clone());
            }
            if let Some(ref container) = details.container_name {
                options.insert("container_name".to_string(), container.clone());
            }
            "az"
        }
        "gcs" | "gs" => "gcs",
        other => {
            return Err(ProxyError::Internal(format!(
                "unsupported provider: {}",
                other
            )))
        }
    }
    .to_string();
    Ok((backend_type, options))
}
