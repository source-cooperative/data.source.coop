//! The main proxy handler that ties together resolution and backend forwarding.
//!
//! [`ProxyHandler`] is generic over the runtime's body type, backend client,
//! and request resolver. This allows it to be used identically on both
//! the server (Tokio/Hyper) and worker (Cloudflare Workers) runtimes.

use crate::backend::{BackendClient, BackendRequest, S3RequestSigner, UNSIGNED_PAYLOAD};
use crate::error::ProxyError;
use crate::resolver::{ResolvedAction, RequestResolver};
use crate::s3::list_rewrite;
use crate::s3::response::ErrorResponse;
use crate::stream::BodyStream;
use crate::types::{BucketConfig, S3Operation};
use bytes::Bytes;
use http::{HeaderMap, Method};
use url::Url;
use uuid::Uuid;

/// The core proxy handler, generic over runtime primitives.
///
/// # Type Parameters
///
/// - `C`: The backend HTTP client for outbound requests to the backing store
/// - `R`: The request resolver that decides what action to take for each request
pub struct ProxyHandler<C, R> {
    client: C,
    resolver: R,
}

impl<C, R> ProxyHandler<C, R>
where
    C: BackendClient,
    R: RequestResolver,
{
    pub fn new(client: C, resolver: R) -> Self {
        Self { client, resolver }
    }

    /// Handle an incoming S3 request.
    ///
    /// This is the main entry point. It:
    /// 1. Resolves the request via the resolver (parse, auth, authorize)
    /// 2. Forwards the request to the backing store or returns a synthetic response
    /// 3. Optionally rewrites list response XML
    pub async fn handle_request(
        &self,
        method: Method,
        path: &str,
        query: Option<&str>,
        headers: &HeaderMap,
        body: C::Body,
    ) -> ProxyResult<C::Body> {
        let request_id = Uuid::new_v4().to_string();

        tracing::info!(
            request_id = %request_id,
            method = %method,
            path = %path,
            query = ?query,
            "incoming request"
        );

        match self
            .handle_inner(method, path, query, headers, body)
            .await
        {
            Ok(resp) => {
                tracing::info!(
                    request_id = %request_id,
                    status = resp.status,
                    "request completed"
                );
                resp
            }
            Err(err) => {
                tracing::warn!(
                    request_id = %request_id,
                    error = %err,
                    status = err.status_code(),
                    s3_code = %err.s3_error_code(),
                    "request failed"
                );
                error_response(&err, path, &request_id)
            }
        }
    }

    async fn handle_inner(
        &self,
        method: Method,
        path: &str,
        query: Option<&str>,
        headers: &HeaderMap,
        body: C::Body,
    ) -> Result<ProxyResult<C::Body>, ProxyError> {
        let action = self.resolver.resolve(&method, path, query, headers).await?;

        match action {
            ResolvedAction::Response {
                status,
                headers: resp_headers,
                body: resp_body,
            } => Ok(ProxyResult {
                status,
                headers: resp_headers,
                body: C::Body::from_bytes(resp_body),
            }),
            ResolvedAction::Proxy {
                operation,
                bucket_config,
                list_rewrite,
            } => {
                self.forward_to_backend(&method, &operation, &bucket_config, headers, body, list_rewrite.as_ref())
                    .await
            }
        }
    }

    async fn forward_to_backend(
        &self,
        method: &Method,
        operation: &S3Operation,
        bucket_config: &BucketConfig,
        original_headers: &HeaderMap,
        body: C::Body,
        list_rewrite: Option<&crate::resolver::ListRewrite>,
    ) -> Result<ProxyResult<C::Body>, ProxyError> {
        // Build the backend URL
        let backend_url = build_backend_url(bucket_config, operation)?;

        tracing::debug!(backend_url = %backend_url, "forwarding request to backend");

        let mut headers = HeaderMap::new();

        // Forward relevant headers
        for header_name in &[
            "content-type",
            "content-length",
            "content-md5",
            "range",
            "if-match",
            "if-none-match",
            "if-modified-since",
            "if-unmodified-since",
        ] {
            if let Some(val) = original_headers.get(*header_name) {
                headers.insert(*header_name, val.clone());
            }
        }

        // Only sign the outbound request if the backend has credentials configured.
        // Public backends (e.g. source.coop) don't need signing.
        let has_credentials = !bucket_config.backend_access_key_id.is_empty()
            && !bucket_config.backend_secret_access_key.is_empty();

        let parsed_url = Url::parse(&backend_url)
            .map_err(|e| ProxyError::Internal(format!("invalid backend URL: {}", e)))?;

        if has_credentials {
            let signer = S3RequestSigner::new(
                bucket_config.backend_access_key_id.clone(),
                bucket_config.backend_secret_access_key.clone(),
                bucket_config.backend_region.clone(),
            );
            signer.sign_request(method, &parsed_url, &mut headers, UNSIGNED_PAYLOAD)?;
            tracing::trace!("outbound request signed with SigV4");
        } else {
            // For unsigned requests, still set the host header
            let host = parsed_url
                .host_str()
                .ok_or_else(|| ProxyError::Internal("no host in URL".into()))?;
            let host_header = if let Some(port) = parsed_url.port() {
                format!("{}:{}", host, port)
            } else {
                host.to_string()
            };
            headers.insert("host", host_header.parse().unwrap());
            tracing::trace!("outbound request unsigned (public backend)");
        }

        let backend_req = BackendRequest {
            method: method.clone(),
            url: backend_url,
            headers,
            body,
        };

        let backend_resp = self.client.send_request(backend_req).await?;

        tracing::debug!(
            status = backend_resp.status,
            "backend response received"
        );

        // Apply list rewrite if configured and this is a successful list response
        if let Some(rewrite) = list_rewrite {
            if matches!(operation, S3Operation::ListBucket { .. })
                && backend_resp.status >= 200
                && backend_resp.status < 300
            {
                // List responses are small XML — safe to buffer
                let body_bytes = backend_resp
                    .body
                    .read_to_bytes()
                    .await
                    .map_err(|e| ProxyError::Internal(format!("failed to read list response: {}", e)))?;
                let xml_str = String::from_utf8_lossy(&body_bytes);
                let rewritten = list_rewrite::rewrite_list_response(&xml_str, rewrite);
                return Ok(ProxyResult {
                    status: backend_resp.status,
                    headers: backend_resp.headers,
                    body: C::Body::from_bytes(Bytes::from(rewritten)),
                });
            }
        }

        Ok(ProxyResult {
            status: backend_resp.status,
            headers: backend_resp.headers,
            body: backend_resp.body,
        })
    }
}

/// The result of handling a proxy request.
pub struct ProxyResult<B> {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: B,
}

fn error_response<B: BodyStream>(err: &ProxyError, resource: &str, request_id: &str) -> ProxyResult<B> {
    let xml = ErrorResponse::from_proxy_error(err, resource, request_id).to_xml();
    let body = B::from_bytes(Bytes::from(xml));
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "application/xml".parse().unwrap());

    ProxyResult {
        status: err.status_code(),
        headers,
        body,
    }
}

fn build_backend_url(
    config: &BucketConfig,
    operation: &S3Operation,
) -> Result<String, ProxyError> {
    let base = config.backend_endpoint.trim_end_matches('/');
    let bucket = &config.backend_bucket;
    let bucket_is_empty = bucket.is_empty();

    let key = match operation {
        S3Operation::GetObject { key, .. }
        | S3Operation::HeadObject { key, .. }
        | S3Operation::PutObject { key, .. }
        | S3Operation::CreateMultipartUpload { key, .. }
        | S3Operation::UploadPart { key, .. }
        | S3Operation::CompleteMultipartUpload { key, .. }
        | S3Operation::AbortMultipartUpload { key, .. } => {
            let mut full_key = String::new();
            if let Some(prefix) = &config.backend_prefix {
                full_key.push_str(prefix.trim_end_matches('/'));
                full_key.push('/');
            }
            full_key.push_str(key);
            full_key
        }
        S3Operation::ListBucket { raw_query, .. } => {
            let base_url = if bucket_is_empty {
                base.to_string()
            } else {
                format!("{}/{}", base, bucket)
            };
            let query_string = build_list_query_string(raw_query.as_deref(), config);
            if query_string.is_empty() {
                return Ok(base_url);
            }
            return Ok(format!("{}?{}", base_url, query_string));
        }
        _ => return Err(ProxyError::Internal("unexpected operation".into())),
    };

    // Build URL: skip bucket segment when backend_bucket is empty (e.g. source.coop
    // where the endpoint itself is the bucket root)
    let mut url = if bucket_is_empty {
        format!("{}/{}", base, key)
    } else {
        format!("{}/{}/{}", base, bucket, key)
    };

    match operation {
        S3Operation::CreateMultipartUpload { .. } => {
            url.push_str("?uploads");
        }
        S3Operation::UploadPart {
            upload_id,
            part_number,
            ..
        } => {
            url.push_str(&format!("?partNumber={}&uploadId={}", part_number, upload_id));
        }
        S3Operation::CompleteMultipartUpload { upload_id, .. }
        | S3Operation::AbortMultipartUpload { upload_id, .. } => {
            url.push_str(&format!("?uploadId={}", upload_id));
        }
        _ => {}
    }

    Ok(url)
}

/// Build the query string for a ListBucket backend request.
///
/// - Forwards all incoming query params verbatim
/// - Prepends `backend_prefix` to the `prefix` param if configured
/// - Injects `list-type=2` if not specified (default to ListObjectsV2)
/// - Injects `max-keys=1000` if not specified
fn build_list_query_string(raw_query: Option<&str>, config: &BucketConfig) -> String {
    let mut params: Vec<(String, String)> = raw_query
        .map(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // Merge backend_prefix into the prefix param
    if let Some(backend_prefix) = &config.backend_prefix {
        let bp = backend_prefix.trim_end_matches('/');
        if !bp.is_empty() {
            if let Some((_k, v)) = params.iter_mut().find(|(k, _)| k == "prefix") {
                // Prepend backend_prefix to the client-supplied prefix
                *v = format!("{}/{}", bp, v);
            } else {
                // No client prefix — set prefix to the backend_prefix (with trailing /)
                // so the list is scoped to the backend_prefix directory
                params.push(("prefix".to_string(), format!("{}/", bp)));
            }
        }
    }

    // Default to ListObjectsV2 if no list-type specified
    if !params.iter().any(|(k, _)| k == "list-type") {
        params.push(("list-type".to_string(), "2".to_string()));
    }

    // Default max-keys to 1000 if not specified
    if !params.iter().any(|(k, _)| k == "max-keys") {
        params.push(("max-keys".to_string(), "1000".to_string()));
    }

    // Re-encode
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params.iter())
        .finish()
}
