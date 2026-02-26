//! [`RequestResolver`] implementation for Source Cooperative.
//!
//! Consolidates all Source Cooperative business logic (URL namespace mapping,
//! external API auth, query/response prefix rewriting, synthetic XML responses)
//! into a single resolver that thin runtime adapters call.

use crate::api::{HttpClient, SourceApiClient};
use bytes::Bytes;
use http::{HeaderMap, Method};
use source_coop_core::error::ProxyError;
use source_coop_core::resolver::{ListRewrite, RequestResolver, ResolvedAction};
use source_coop_core::s3::request::build_s3_operation;
use source_coop_core::types::BucketConfig;
use std::collections::HashMap;

/// Request resolver for Source Cooperative.
///
/// Routes requests based on the URL namespace:
/// - `/` -> synthetic empty ListBuckets
/// - `/{account_id}` -> synthetic account listing or list-with-prefix
/// - `/{account_id}/{repo_id}[/{key}]` -> proxy to backend
#[derive(Clone)]
pub struct SourceCoopResolver<H: HttpClient> {
    api_client: SourceApiClient<H>,
}

impl<H: HttpClient> SourceCoopResolver<H> {
    pub fn new(api_client: SourceApiClient<H>) -> Self {
        Self { api_client }
    }

    /// Resolve a bucket config from the Source API for a given account/repo.
    async fn resolve_bucket_config(
        &self,
        account_id: &str,
        repo_id: &str,
    ) -> Result<BucketConfig, ProxyError> {
        let bucket_name = format!("{}/{}", account_id, repo_id);

        let product = self
            .api_client
            .get_product(account_id, repo_id)
            .await?
            .ok_or_else(|| ProxyError::BucketNotFound(bucket_name.clone()))?;

        if product.disabled {
            return Err(ProxyError::BucketNotFound(bucket_name));
        }

        let mirror = product
            .metadata
            .mirrors
            .get(&product.metadata.primary_mirror)
            .ok_or_else(|| {
                ProxyError::ConfigError(format!(
                    "primary mirror '{}' not found in product mirrors",
                    product.metadata.primary_mirror
                ))
            })?;

        let conn = self
            .api_client
            .get_data_connection(&mirror.connection_id)
            .await?
            .ok_or_else(|| {
                ProxyError::ConfigError(format!(
                    "data connection '{}' not found",
                    mirror.connection_id
                ))
            })?;

        let base_prefix = conn.details.base_prefix.unwrap_or_default();

        let backend_prefix = {
            let bp = base_prefix.trim_end_matches('/');
            let mp = mirror.prefix.trim_end_matches('/');
            if bp.is_empty() && mp.is_empty() {
                None
            } else if bp.is_empty() {
                Some(mp.to_string())
            } else if mp.is_empty() {
                Some(bp.to_string())
            } else {
                Some(format!("{}/{}", bp, mp))
            }
        };

        let provider = conn.details.provider.as_str();
        let backend_options = match provider {
            "s3" => {
                let region = conn.details.region.unwrap_or_else(|| "us-east-1".into());
                let bucket = conn.details.bucket.unwrap_or_default();
                let endpoint = format!("https://s3.{}.amazonaws.com", region);
                let mut opts = HashMap::new();
                opts.insert("endpoint".to_string(), endpoint);
                opts.insert("bucket_name".to_string(), bucket);
                opts.insert("region".to_string(), region);
                if let Some(ref auth) = conn.authentication {
                    if let Some(ak) = &auth.access_key_id {
                        opts.insert("access_key_id".to_string(), ak.clone());
                    }
                    if let Some(sk) = &auth.secret_access_key {
                        opts.insert("secret_access_key".to_string(), sk.clone());
                    }
                    if auth.access_key_id.is_none() {
                        opts.insert("skip_signature".to_string(), "true".to_string());
                    }
                } else {
                    opts.insert("skip_signature".to_string(), "true".to_string());
                }
                opts
            }
            "az" | "azure" => {
                let mut opts = HashMap::new();
                if let Some(name) = &conn.details.account_name {
                    opts.insert("account_name".to_string(), name.clone());
                }
                if let Some(container) = &conn.details.container_name {
                    opts.insert("container_name".to_string(), container.clone());
                }
                if let Some(ref auth) = conn.authentication {
                    if let Some(key) = &auth.access_key {
                        opts.insert("access_key".to_string(), key.clone());
                    }
                    if auth.access_key.is_none() {
                        opts.insert("skip_signature".to_string(), "true".to_string());
                    }
                } else {
                    opts.insert("skip_signature".to_string(), "true".to_string());
                }
                opts
            }
            other => {
                return Err(ProxyError::ConfigError(format!(
                    "unsupported provider '{}' for data connection",
                    other
                )));
            }
        };

        let backend_type = match provider {
            "az" | "azure" => "az".to_string(),
            other => other.to_string(),
        };

        Ok(BucketConfig {
            name: bucket_name,
            backend_type,
            backend_prefix,
            anonymous_access: product.data_mode == "open",
            allowed_roles: vec![],
            backend_options,
        })
    }

    /// Check permissions for an authenticated user via the Source API.
    async fn check_permissions(
        &self,
        headers: &HeaderMap,
        account_id: &str,
        repo_id: &str,
        method: &Method,
    ) -> Result<(), ProxyError> {
        let auth_header = match headers.get("authorization").and_then(|v| v.to_str().ok()) {
            Some(h) => h,
            None => return Ok(()), // Anonymous — skip permission check
        };

        let sig = source_coop_core::auth::parse_sigv4_auth(auth_header)?;

        let perms = self
            .api_client
            .get_permissions(account_id, repo_id, &sig.access_key_id)
            .await?
            .ok_or(ProxyError::AccessDenied)?;

        let is_write = matches!(*method, Method::PUT | Method::POST | Method::DELETE);

        if is_write && !perms.write {
            tracing::warn!(
                account_id = account_id,
                repo_id = repo_id,
                access_key_id = %sig.access_key_id,
                "write permission denied by Source API"
            );
            return Err(ProxyError::AccessDenied);
        }

        if !is_write && !perms.read {
            tracing::warn!(
                account_id = account_id,
                repo_id = repo_id,
                access_key_id = %sig.access_key_id,
                "read permission denied by Source API"
            );
            return Err(ProxyError::AccessDenied);
        }

        Ok(())
    }

    /// Handle `GET /{account_id}` — synthetic account listing.
    async fn handle_account_listing(&self, account_id: &str) -> Result<ResolvedAction, ProxyError> {
        tracing::info!(
            account_id = account_id,
            "handling account listing for account"
        );
        let account = self
            .api_client
            .list_account_repos(account_id)
            .await?
            .ok_or_else(|| ProxyError::BucketNotFound(account_id.to_string()))?;

        tracing::info!(
            account_id = account_id,
            repo_count = account.products.len(),
            "fetched account listing from Source API"
        );

        let prefixes: Vec<String> = account
            .products
            .iter()
            .map(|p| format!("{}/", p.product_id))
            .collect();

        let xml = synthetic_list_objects_v2_xml(account_id, &prefixes);
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/xml".parse().unwrap());

        Ok(ResolvedAction::Response {
            status: 200,
            headers,
            body: Bytes::from(xml),
        })
    }

    /// Handle a list-with-prefix request where the prefix includes a repo_id.
    ///
    /// `GET /{account_id}?prefix=repo_id/subdir/...`
    async fn handle_list_with_prefix(
        &self,
        method: &Method,
        headers: &HeaderMap,
        query: &str,
        account_id: &str,
        repo_id: &str,
    ) -> Result<ResolvedAction, ProxyError> {
        let bucket_name = format!("{}/{}", account_id, repo_id);

        // Permission check
        self.check_permissions(headers, account_id, repo_id, method)
            .await?;

        // Rewrite query: strip `{repo_id}/` from the prefix param
        let new_query = rewrite_list_prefix(query, repo_id);

        let bucket_config = self.resolve_bucket_config(account_id, repo_id).await?;

        let operation = build_s3_operation(method, bucket_name, String::new(), Some(&new_query))?;

        // Build list rewrite: strip backend prefix, add repo_id
        let list_rewrite = build_list_rewrite(&bucket_config, repo_id);

        Ok(ResolvedAction::Proxy {
            operation,
            bucket_config,
            list_rewrite,
        })
    }
}

impl<H: HttpClient> RequestResolver for SourceCoopResolver<H> {
    async fn resolve(
        &self,
        method: &Method,
        path: &str,
        query: Option<&str>,
        headers: &HeaderMap,
    ) -> Result<ResolvedAction, ProxyError> {
        let trimmed = path.trim_start_matches('/');
        let segments: Vec<&str> = trimmed.splitn(3, '/').collect();

        match segments.as_slice() {
            // Root: GET / -> empty ListBuckets
            [] | [""] => {
                let xml = empty_list_buckets_xml();
                let mut resp_headers = HeaderMap::new();
                resp_headers.insert("content-type", "application/xml".parse().unwrap());
                Ok(ResolvedAction::Response {
                    status: 200,
                    headers: resp_headers,
                    body: Bytes::from(xml),
                })
            }

            // /{account_id} — either account listing or list-with-prefix
            [account_id] if !account_id.is_empty() => {
                // Check if there's a prefix query param that starts with a repo_id
                if let Some(q) = query {
                    if let Some(prefix) = extract_query_param(q, "prefix") {
                        if let Some((repo_part, _rest)) = prefix.split_once('/') {
                            if !repo_part.is_empty() {
                                return self
                                    .handle_list_with_prefix(
                                        method, headers, q, account_id, repo_part,
                                    )
                                    .await;
                            }
                        }
                    }
                }

                // No prefix or prefix doesn't contain repo -> synthetic account listing
                self.handle_account_listing(account_id).await
            }

            // /{account_id}/{repo_id} or /{account_id}/{repo_id}/{key...}
            [account_id, repo_id_and_rest @ ..] if !account_id.is_empty() => {
                let (repo_id, key) = if repo_id_and_rest.len() == 1 {
                    (repo_id_and_rest[0], "")
                } else {
                    (repo_id_and_rest[0], repo_id_and_rest[1])
                };

                if repo_id.is_empty() {
                    return Err(ProxyError::InvalidRequest("empty repo_id".into()));
                }

                let bucket_name = format!("{}/{}", account_id, repo_id);

                // Permission check via Source API
                self.check_permissions(headers, account_id, repo_id, method)
                    .await?;

                // Resolve bucket config
                let bucket_config = self.resolve_bucket_config(account_id, repo_id).await?;

                // Build the S3 operation
                let operation = build_s3_operation(method, bucket_name, key.to_string(), query)?;

                // For list operations, apply list rewrite
                let list_rewrite = if key.is_empty() {
                    build_list_rewrite(&bucket_config, repo_id)
                } else {
                    None
                };

                Ok(ResolvedAction::Proxy {
                    operation,
                    bucket_config,
                    list_rewrite,
                })
            }

            _ => Err(ProxyError::InvalidRequest("invalid path".into())),
        }
    }
}

// -- Helpers --

/// Build a [`ListRewrite`] that strips the backend prefix and prepends `repo_id`.
fn build_list_rewrite(bucket_config: &BucketConfig, repo_id: &str) -> Option<ListRewrite> {
    let strip = bucket_config
        .backend_prefix
        .as_deref()
        .unwrap_or("")
        .trim_end_matches('/');

    if strip.is_empty() && repo_id.is_empty() {
        return None;
    }

    Some(ListRewrite {
        strip_prefix: if strip.is_empty() {
            String::new()
        } else {
            format!("{}/", strip)
        },
        add_prefix: repo_id.to_string(),
    })
}

fn empty_list_buckets_xml() -> String {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<ListAllMyBucketsResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Owner><ID>source-coop</ID><DisplayName>source-coop</DisplayName></Owner>
  <Buckets/>
</ListAllMyBucketsResult>"#
        .to_string()
}

fn synthetic_list_objects_v2_xml(bucket: &str, common_prefixes: &[String]) -> String {
    let mut xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Name>{}</Name>
  <Prefix/>
  <KeyCount>{}</KeyCount>
  <MaxKeys>1000</MaxKeys>
  <IsTruncated>false</IsTruncated>"#,
        bucket,
        common_prefixes.len()
    );

    for prefix in common_prefixes {
        xml.push_str(&format!(
            "\n  <CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>",
            prefix
        ));
    }

    xml.push_str("\n</ListBucketResult>");
    xml
}

fn extract_query_param(query: &str, name: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.to_string())
}

fn rewrite_list_prefix(query: &str, repo_id: &str) -> String {
    let params: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| {
            if k == "prefix" {
                let prefix_to_strip = format!("{}/", repo_id);
                let new_v = v.strip_prefix(&prefix_to_strip).unwrap_or(&v).to_string();
                (k.to_string(), new_v)
            } else {
                (k.to_string(), v.to_string())
            }
        })
        .collect();

    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params.iter())
        .finish()
}
