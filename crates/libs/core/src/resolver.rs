//! Request resolution abstraction.
//!
//! The [`RequestResolver`] trait decouples "what to do with a request" from
//! the proxy handler itself. Each product (static config, Source Cooperative,
//! etc.) implements its own resolver. The proxy handler simply calls
//! `resolver.resolve()` and acts on the [`ResolvedAction`].

use crate::auth;
use crate::config::ConfigProvider;
use crate::error::ProxyError;
use crate::maybe_send::{MaybeSend, MaybeSync};
use crate::s3::request::{self, HostStyle};
use crate::s3::response::{BucketEntry, BucketList, BucketOwner, ListAllMyBucketsResult};
use crate::types::{BucketConfig, S3Operation};
use bytes::Bytes;
use http::{HeaderMap, Method};
use std::future::Future;

/// Trait for resolving an incoming request into an action the proxy should take.
///
/// Implementations encapsulate namespace mapping, authentication, authorization,
/// and any request rewriting logic specific to a product or deployment mode.
pub trait RequestResolver: Clone + MaybeSend + MaybeSync + 'static {
    fn resolve(
        &self,
        method: &Method,
        path: &str,
        query: Option<&str>,
        headers: &HeaderMap,
    ) -> impl Future<Output = Result<ResolvedAction, ProxyError>> + MaybeSend;
}

/// The action the proxy handler should take after resolution.
pub enum ResolvedAction {
    /// Forward the request to a backend. Core handles URL building, signing, streaming.
    Proxy {
        operation: S3Operation,
        bucket_config: BucketConfig,
        /// Optional rewrite rule for list response XML.
        list_rewrite: Option<ListRewrite>,
    },
    /// Return a synthetic response directly (small XML, never a stream).
    Response {
        status: u16,
        headers: HeaderMap,
        body: Bytes,
    },
}

/// Describes how to rewrite `<Key>` and `<Prefix>` values in list response XML.
#[derive(Debug, Clone)]
pub struct ListRewrite {
    /// Prefix to strip from the beginning of values.
    pub strip_prefix: String,
    /// Prefix to add after stripping.
    pub add_prefix: String,
}

/// Default resolver backed by a [`ConfigProvider`].
///
/// Extracts the S3 operation from the request, looks up the bucket in the
/// config, authenticates and authorizes, then returns a [`ResolvedAction::Proxy`].
/// `ListBuckets` is handled as a synthetic [`ResolvedAction::Response`].
#[derive(Clone)]
pub struct DefaultResolver<P> {
    config: P,
    virtual_host_domain: Option<String>,
}

impl<P> DefaultResolver<P> {
    pub fn new(config: P, virtual_host_domain: Option<String>) -> Self {
        Self {
            config,
            virtual_host_domain,
        }
    }
}

impl<P: ConfigProvider> RequestResolver for DefaultResolver<P> {
    async fn resolve(
        &self,
        method: &Method,
        path: &str,
        query: Option<&str>,
        headers: &HeaderMap,
    ) -> Result<ResolvedAction, ProxyError> {
        // Determine host style
        let host_style = determine_host_style(headers, self.virtual_host_domain.as_deref());

        // Parse the S3 operation
        let operation = request::parse_s3_request(method, path, query, headers, host_style)?;
        tracing::debug!(operation = ?operation, "parsed S3 operation");

        // Handle STS requests separately (no bucket lookup needed)
        if let S3Operation::AssumeRoleWithWebIdentity { .. } = &operation {
            tracing::info!("STS AssumeRoleWithWebIdentity request");
            return Err(ProxyError::InvalidRequest(
                "STS endpoint: use s3-proxy-auth crate for OIDC token exchange".into(),
            ));
        }

        // Handle ListBuckets — returns virtual bucket list from config, no backend call
        if matches!(operation, S3Operation::ListBuckets) {
            let buckets = self.config.list_buckets().await?;
            tracing::info!(count = buckets.len(), "listing virtual buckets");
            let xml = ListAllMyBucketsResult {
                owner: BucketOwner {
                    id: "s3-proxy".to_string(),
                    display_name: "s3-proxy".to_string(),
                },
                buckets: BucketList {
                    buckets: buckets
                        .iter()
                        .map(|b| BucketEntry {
                            name: b.name.clone(),
                            creation_date: "2024-01-01T00:00:00.000Z".to_string(),
                        })
                        .collect(),
                },
            }
            .to_xml();

            let mut resp_headers = HeaderMap::new();
            resp_headers.insert("content-type", "application/xml".parse().unwrap());
            return Ok(ResolvedAction::Response {
                status: 200,
                headers: resp_headers,
                body: Bytes::from(xml),
            });
        }

        // Get bucket name and look up config
        let bucket_name = operation.bucket()
            .ok_or_else(|| ProxyError::InvalidRequest("no bucket in request".into()))?;

        let bucket_config = self
            .config
            .get_bucket(bucket_name)
            .await?
            .ok_or_else(|| {
                tracing::warn!(bucket = %bucket_name, "bucket not found in config");
                ProxyError::BucketNotFound(bucket_name.to_string())
            })?;

        tracing::debug!(
            bucket = %bucket_name,
            backend_type = %bucket_config.backend_type,
            "resolved bucket config"
        );

        // Authenticate
        let identity = auth::resolve_identity(headers, &self.config).await?;
        tracing::debug!(identity = ?identity, "resolved identity");

        // Authorize
        auth::authorize(&identity, &operation, &bucket_config)?;
        tracing::trace!("authorization passed");

        Ok(ResolvedAction::Proxy {
            operation,
            bucket_config,
            list_rewrite: None,
        })
    }
}

fn determine_host_style(headers: &HeaderMap, virtual_host_domain: Option<&str>) -> HostStyle {
    if let Some(domain) = virtual_host_domain {
        if let Some(host) = headers.get("host").and_then(|v| v.to_str().ok()) {
            let host = host.split(':').next().unwrap_or(host);
            if let Some(bucket) = host.strip_suffix(&format!(".{}", domain)) {
                return HostStyle::VirtualHosted {
                    bucket: bucket.to_string(),
                };
            }
        }
    }
    HostStyle::Path
}
