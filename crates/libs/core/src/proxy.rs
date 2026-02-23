//! The main proxy handler that ties together resolution and backend forwarding.
//!
//! [`ProxyHandler`] is generic over the runtime's backend and request resolver.
//! It uses a two-phase dispatch model:
//!
//! 1. **`resolve_request`** — parses, authenticates, and decides the action:
//!    - GET/HEAD/PUT/DELETE → [`HandlerAction::Forward`] with a presigned URL
//!    - LIST → [`HandlerAction::Response`] with XML body
//!    - Multipart → [`HandlerAction::NeedsBody`] (body required)
//!    - Errors/synthetic → [`HandlerAction::Response`]
//!
//! 2. **`handle_with_body`** — completes multipart operations once the body arrives.
//!
//! Runtimes handle [`Forward`] by executing the presigned URL with their native
//! HTTP client, enabling zero-copy streaming for both request and response bodies.

use crate::backend::{hash_payload, ProxyBackend, S3RequestSigner, UNSIGNED_PAYLOAD};
use crate::error::ProxyError;
use crate::resolver::{ListRewrite, RequestResolver, ResolvedAction};
use crate::response_body::ProxyResponseBody;
use crate::s3::response::{
    ErrorResponse, ListBucketResult, ListCommonPrefix, ListContents,
};
use crate::types::{BucketConfig, S3Operation};
use bytes::Bytes;
use http::{HeaderMap, Method};
use object_store::ObjectStore;
use std::time::Duration;
use url::Url;
use uuid::Uuid;

/// TTL for presigned URLs. Short because they're used immediately.
const PRESIGNED_URL_TTL: Duration = Duration::from_secs(300);

/// The action the handler wants the runtime to take.
pub enum HandlerAction {
    /// A fully formed response (LIST results, errors, synthetic responses).
    Response(ProxyResult),
    /// A presigned URL for the runtime to execute with its native HTTP client.
    /// The runtime streams request/response bodies directly — no handler involvement.
    Forward(ForwardRequest),
    /// The handler needs the request body to continue (multipart operations).
    /// The runtime should materialize the body and call `handle_with_body`.
    NeedsBody(PendingRequest),
}

/// A presigned URL request for the runtime to execute.
pub struct ForwardRequest {
    /// HTTP method for the backend request.
    pub method: Method,
    /// Presigned URL to the backend (includes auth in query params).
    pub url: Url,
    /// Headers to include in the backend request (Range, If-Match, Content-Type, etc.).
    pub headers: HeaderMap,
}

/// Opaque state for a multipart operation that needs the request body.
pub struct PendingRequest {
    method: Method,
    operation: S3Operation,
    bucket_config: BucketConfig,
    original_headers: HeaderMap,
    request_id: String,
}

/// The core proxy handler, generic over runtime primitives.
///
/// # Type Parameters
///
/// - `B`: The runtime's backend for object store creation, signing, and raw HTTP
/// - `R`: The request resolver that decides what action to take for each request
pub struct ProxyHandler<B, R> {
    backend: B,
    resolver: R,
}

impl<B, R> ProxyHandler<B, R>
where
    B: ProxyBackend,
    R: RequestResolver,
{
    pub fn new(backend: B, resolver: R) -> Self {
        Self { backend, resolver }
    }

    /// Phase 1: Resolve an incoming request into an action.
    ///
    /// This is the main entry point. It:
    /// 1. Resolves the request via the resolver (parse, auth, authorize)
    /// 2. Determines what the runtime should do next:
    ///    - `Forward` a presigned URL (GET/HEAD/PUT/DELETE)
    ///    - Return a `Response` directly (LIST, errors, synthetic)
    ///    - Request the body via `NeedsBody` (multipart)
    pub async fn resolve_request(
        &self,
        method: Method,
        path: &str,
        query: Option<&str>,
        headers: &HeaderMap,
    ) -> HandlerAction {
        let request_id = Uuid::new_v4().to_string();

        tracing::info!(
            request_id = %request_id,
            method = %method,
            path = %path,
            query = ?query,
            "incoming request"
        );

        match self
            .resolve_inner(method, path, query, headers, &request_id)
            .await
        {
            Ok(action) => {
                match &action {
                    HandlerAction::Response(resp) => {
                        tracing::info!(
                            request_id = %request_id,
                            status = resp.status,
                            "request completed"
                        );
                    }
                    HandlerAction::Forward(fwd) => {
                        tracing::info!(
                            request_id = %request_id,
                            method = %fwd.method,
                            "forwarding via presigned URL"
                        );
                    }
                    HandlerAction::NeedsBody(_) => {
                        tracing::debug!(
                            request_id = %request_id,
                            "request needs body (multipart)"
                        );
                    }
                }
                action
            }
            Err(err) => {
                tracing::warn!(
                    request_id = %request_id,
                    error = %err,
                    status = err.status_code(),
                    s3_code = %err.s3_error_code(),
                    "request failed"
                );
                HandlerAction::Response(error_response(&err, path, &request_id))
            }
        }
    }

    /// Phase 2: Complete a multipart operation with the request body.
    ///
    /// Called by the runtime after materializing the body for a `NeedsBody` action.
    pub async fn handle_with_body(
        &self,
        pending: PendingRequest,
        body: Bytes,
    ) -> ProxyResult {
        match self.execute_multipart(&pending, body).await {
            Ok(result) => {
                tracing::info!(
                    request_id = %pending.request_id,
                    status = result.status,
                    "multipart request completed"
                );
                result
            }
            Err(err) => {
                tracing::warn!(
                    request_id = %pending.request_id,
                    error = %err,
                    status = err.status_code(),
                    s3_code = %err.s3_error_code(),
                    "multipart request failed"
                );
                error_response(&err, pending.operation.key(), &pending.request_id)
            }
        }
    }

    async fn resolve_inner(
        &self,
        method: Method,
        path: &str,
        query: Option<&str>,
        headers: &HeaderMap,
        request_id: &str,
    ) -> Result<HandlerAction, ProxyError> {
        let action = self.resolver.resolve(&method, path, query, headers).await?;

        match action {
            ResolvedAction::Response {
                status,
                headers: resp_headers,
                body: resp_body,
            } => Ok(HandlerAction::Response(ProxyResult {
                status,
                headers: resp_headers,
                body: ProxyResponseBody::from_bytes(resp_body),
            })),
            ResolvedAction::Proxy {
                operation,
                bucket_config,
                list_rewrite,
            } => {
                self.dispatch_operation(
                    &method,
                    &operation,
                    &bucket_config,
                    headers,
                    list_rewrite.as_ref(),
                    request_id,
                )
                .await
            }
        }
    }

    async fn dispatch_operation(
        &self,
        method: &Method,
        operation: &S3Operation,
        bucket_config: &BucketConfig,
        original_headers: &HeaderMap,
        list_rewrite: Option<&ListRewrite>,
        request_id: &str,
    ) -> Result<HandlerAction, ProxyError> {
        match operation {
            S3Operation::GetObject { key, .. } => {
                let fwd = self.build_forward(
                    Method::GET,
                    bucket_config,
                    key,
                    original_headers,
                    &["range", "if-match", "if-none-match", "if-modified-since", "if-unmodified-since"],
                ).await?;
                tracing::debug!(url = %fwd.url, "GET via presigned URL");
                Ok(HandlerAction::Forward(fwd))
            }
            S3Operation::HeadObject { key, .. } => {
                let fwd = self.build_forward(
                    Method::HEAD,
                    bucket_config,
                    key,
                    original_headers,
                    &["if-match", "if-none-match", "if-modified-since", "if-unmodified-since"],
                ).await?;
                tracing::debug!(url = %fwd.url, "HEAD via presigned URL");
                Ok(HandlerAction::Forward(fwd))
            }
            S3Operation::PutObject { key, .. } => {
                let fwd = self.build_forward(
                    Method::PUT,
                    bucket_config,
                    key,
                    original_headers,
                    &["content-type", "content-length", "content-md5"],
                ).await?;
                tracing::debug!(url = %fwd.url, "PUT via presigned URL");
                Ok(HandlerAction::Forward(fwd))
            }
            S3Operation::DeleteObject { key, .. } => {
                let fwd = self.build_forward(
                    Method::DELETE,
                    bucket_config,
                    key,
                    original_headers,
                    &[],
                ).await?;
                tracing::debug!(url = %fwd.url, "DELETE via presigned URL");
                Ok(HandlerAction::Forward(fwd))
            }
            S3Operation::ListBucket { raw_query, .. } => {
                let result = self
                    .handle_list(bucket_config, raw_query.as_deref(), list_rewrite)
                    .await?;
                Ok(HandlerAction::Response(result))
            }
            // Multipart operations need the request body
            S3Operation::CreateMultipartUpload { .. }
            | S3Operation::UploadPart { .. }
            | S3Operation::CompleteMultipartUpload { .. }
            | S3Operation::AbortMultipartUpload { .. } => {
                if !bucket_config.supports_s3_multipart() {
                    return Err(ProxyError::InvalidRequest(format!(
                        "multipart operations not supported for '{}' backends",
                        bucket_config.backend_type
                    )));
                }
                Ok(HandlerAction::NeedsBody(PendingRequest {
                    method: method.clone(),
                    operation: operation.clone(),
                    bucket_config: bucket_config.clone(),
                    original_headers: original_headers.clone(),
                    request_id: request_id.to_string(),
                }))
            }
            _ => Err(ProxyError::Internal("unexpected operation".into())),
        }
    }

    /// Build a [`ForwardRequest`] with a presigned URL for the given operation.
    async fn build_forward(
        &self,
        method: Method,
        config: &BucketConfig,
        key: &str,
        original_headers: &HeaderMap,
        forward_header_names: &[&'static str],
    ) -> Result<ForwardRequest, ProxyError> {
        let signer = self.backend.create_signer(config)?;
        let path = build_object_path(config, key);

        let url = signer
            .signed_url(method.clone(), &path, PRESIGNED_URL_TTL)
            .await
            .map_err(ProxyError::from_object_store_error)?;

        let mut fwd_headers = HeaderMap::new();
        for name in forward_header_names {
            if let Some(v) = original_headers.get(*name) {
                fwd_headers.insert(*name, v.clone());
            }
        }

        Ok(ForwardRequest {
            method,
            url,
            headers: fwd_headers,
        })
    }

    /// LIST via object_store
    async fn handle_list(
        &self,
        config: &BucketConfig,
        raw_query: Option<&str>,
        list_rewrite: Option<&ListRewrite>,
    ) -> Result<ProxyResult, ProxyError> {
        let store = self.backend.create_store(config)?;

        // Extract prefix from query string
        let client_prefix = raw_query
            .and_then(|q| {
                url::form_urlencoded::parse(q.as_bytes())
                    .find(|(k, _)| k == "prefix")
                    .map(|(_, v)| v.to_string())
            })
            .unwrap_or_default();

        // Extract delimiter from query string (default "/")
        let delimiter = raw_query
            .and_then(|q| {
                url::form_urlencoded::parse(q.as_bytes())
                    .find(|(k, _)| k == "delimiter")
                    .map(|(_, v)| v.to_string())
            })
            .unwrap_or_else(|| "/".to_string());

        // Build the full prefix including backend_prefix
        let full_prefix = build_list_prefix(config, &client_prefix);

        tracing::debug!(
            full_prefix = %full_prefix,
            delimiter = %delimiter,
            "LIST via object_store"
        );

        let prefix_path = if full_prefix.is_empty() {
            None
        } else {
            Some(object_store::path::Path::from(full_prefix.as_str()))
        };

        let list_result = store
            .list_with_delimiter(prefix_path.as_ref())
            .await
            .map_err(ProxyError::from_object_store_error)?;

        // Build S3 XML response from ListResult
        let bucket_name = &config.name;
        let xml = build_list_xml(
            bucket_name,
            &client_prefix,
            &delimiter,
            &list_result,
            config,
            list_rewrite,
        );

        let mut resp_headers = HeaderMap::new();
        resp_headers.insert("content-type", "application/xml".parse().unwrap());

        Ok(ProxyResult {
            status: 200,
            headers: resp_headers,
            body: ProxyResponseBody::Bytes(Bytes::from(xml)),
        })
    }

    /// Execute a multipart operation via raw signed HTTP.
    async fn execute_multipart(
        &self,
        pending: &PendingRequest,
        body: Bytes,
    ) -> Result<ProxyResult, ProxyError> {
        let backend_url = build_backend_url(&pending.bucket_config, &pending.operation)?;

        tracing::debug!(backend_url = %backend_url, "multipart via raw HTTP");

        let mut headers = HeaderMap::new();

        // Forward relevant headers
        for header_name in &[
            "content-type",
            "content-length",
            "content-md5",
        ] {
            if let Some(val) = pending.original_headers.get(*header_name) {
                headers.insert(*header_name, val.clone());
            }
        }

        let payload_hash = if body.is_empty() {
            UNSIGNED_PAYLOAD.to_string()
        } else {
            hash_payload(&body)
        };

        sign_s3_request(&pending.method, &backend_url, &mut headers, &pending.bucket_config, &payload_hash)?;

        let raw_resp = self
            .backend
            .send_raw(pending.method.clone(), backend_url, headers, body)
            .await?;

        tracing::debug!(status = raw_resp.status, "multipart backend response");

        Ok(ProxyResult {
            status: raw_resp.status,
            headers: raw_resp.headers,
            body: ProxyResponseBody::from_bytes(raw_resp.body),
        })
    }
}

/// The result of handling a proxy request.
pub struct ProxyResult {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: ProxyResponseBody,
}

/// Headers to forward from backend responses (used by runtimes for Forward responses).
pub const RESPONSE_HEADER_ALLOWLIST: &[&str] = &[
    "content-type",
    "content-length",
    "content-range",
    "etag",
    "last-modified",
    "accept-ranges",
    "content-encoding",
    "content-disposition",
    "cache-control",
    "x-amz-request-id",
    "x-amz-version-id",
    "location",
];

fn error_response(err: &ProxyError, resource: &str, request_id: &str) -> ProxyResult {
    let xml = ErrorResponse::from_proxy_error(err, resource, request_id).to_xml();
    let body = ProxyResponseBody::from_bytes(Bytes::from(xml));
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "application/xml".parse().unwrap());

    ProxyResult {
        status: err.status_code(),
        headers,
        body,
    }
}

/// Sign an outbound S3 request using credentials from the bucket config.
///
/// Used for multipart operations only. CRUD operations use presigned URLs.
fn sign_s3_request(
    method: &Method,
    url: &str,
    headers: &mut HeaderMap,
    config: &BucketConfig,
    payload_hash: &str,
) -> Result<(), ProxyError> {
    let access_key = config.option("access_key_id").unwrap_or("");
    let secret_key = config.option("secret_access_key").unwrap_or("");
    let region = config.option("region").unwrap_or("us-east-1");
    let has_credentials = !access_key.is_empty() && !secret_key.is_empty();

    let parsed_url = Url::parse(url)
        .map_err(|e| ProxyError::Internal(format!("invalid backend URL: {}", e)))?;

    if has_credentials {
        let signer = S3RequestSigner::new(
            access_key.to_string(),
            secret_key.to_string(),
            region.to_string(),
        );
        signer.sign_request(method, &parsed_url, headers, payload_hash)?;
    } else {
        let host = parsed_url
            .host_str()
            .ok_or_else(|| ProxyError::Internal("no host in URL".into()))?;
        let host_header = if let Some(port) = parsed_url.port() {
            format!("{}:{}", host, port)
        } else {
            host.to_string()
        };
        headers.insert("host", host_header.parse().unwrap());
    }

    Ok(())
}

/// Build an object_store Path from a bucket config and client-visible key.
fn build_object_path(config: &BucketConfig, key: &str) -> object_store::path::Path {
    let mut full_key = String::new();
    if let Some(prefix) = &config.backend_prefix {
        let p = prefix.trim_end_matches('/');
        if !p.is_empty() {
            full_key.push_str(p);
            full_key.push('/');
        }
    }
    full_key.push_str(key);
    object_store::path::Path::from(full_key)
}

/// Build the full list prefix including backend_prefix.
fn build_list_prefix(config: &BucketConfig, client_prefix: &str) -> String {
    let mut full_prefix = String::new();
    if let Some(bp) = &config.backend_prefix {
        let bp = bp.trim_end_matches('/');
        if !bp.is_empty() {
            full_prefix.push_str(bp);
            full_prefix.push('/');
        }
    }
    full_prefix.push_str(client_prefix);
    full_prefix
}

/// Build S3 ListObjectsV2 XML from an object_store ListResult.
fn build_list_xml(
    bucket_name: &str,
    client_prefix: &str,
    delimiter: &str,
    list_result: &object_store::ListResult,
    config: &BucketConfig,
    list_rewrite: Option<&ListRewrite>,
) -> String {
    let backend_prefix = config
        .backend_prefix
        .as_deref()
        .unwrap_or("")
        .trim_end_matches('/');
    let strip_prefix = if backend_prefix.is_empty() {
        String::new()
    } else {
        format!("{}/", backend_prefix)
    };

    let contents: Vec<ListContents> = list_result
        .objects
        .iter()
        .map(|obj| {
            let raw_key = obj.location.to_string();
            ListContents {
                key: rewrite_key(&raw_key, &strip_prefix, list_rewrite),
                last_modified: obj.last_modified.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
                etag: obj.e_tag.as_deref().unwrap_or("\"\"").to_string(),
                size: obj.size,
                storage_class: "STANDARD",
            }
        })
        .collect();

    let common_prefixes: Vec<ListCommonPrefix> = list_result
        .common_prefixes
        .iter()
        .map(|p| {
            let raw_prefix = format!("{}/", p);
            ListCommonPrefix {
                prefix: rewrite_key(&raw_prefix, &strip_prefix, list_rewrite),
            }
        })
        .collect();

    ListBucketResult {
        xmlns: "http://s3.amazonaws.com/doc/2006-03-01/",
        name: bucket_name.to_string(),
        prefix: client_prefix.to_string(),
        delimiter: delimiter.to_string(),
        max_keys: 1000,
        is_truncated: false,
        key_count: contents.len() + common_prefixes.len(),
        contents,
        common_prefixes,
    }
    .to_xml()
}

/// Apply strip/add prefix rewriting to a key or prefix value.
fn rewrite_key(raw: &str, strip_prefix: &str, list_rewrite: Option<&ListRewrite>) -> String {
    let mut key = raw.to_string();

    // Strip the backend prefix
    if !strip_prefix.is_empty() {
        if let Some(stripped) = key.strip_prefix(strip_prefix) {
            key = stripped.to_string();
        }
    }

    // Apply list_rewrite if present
    if let Some(rewrite) = list_rewrite {
        if !rewrite.strip_prefix.is_empty() {
            if let Some(stripped) = key.strip_prefix(&rewrite.strip_prefix) {
                key = stripped.to_string();
            }
        }
        if !rewrite.add_prefix.is_empty() {
            if key.is_empty() || key.starts_with('/') {
                key = format!("{}{}", rewrite.add_prefix, key);
            } else {
                key = format!("{}/{}", rewrite.add_prefix, key);
            }
        }
    }

    key
}

/// Build the backend URL for an S3 operation.
///
/// Used for multipart operations that go through raw signed HTTP.
pub fn build_backend_url(
    config: &BucketConfig,
    operation: &S3Operation,
) -> Result<String, ProxyError> {
    let endpoint = config.option("endpoint").unwrap_or("");
    let base = endpoint.trim_end_matches('/');
    let bucket = config.option("bucket_name").unwrap_or("");
    let bucket_is_empty = bucket.is_empty();

    let mut key = String::new();
    if let Some(prefix) = &config.backend_prefix {
        key.push_str(prefix.trim_end_matches('/'));
        key.push('/');
    }
    key.push_str(operation.key());

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
