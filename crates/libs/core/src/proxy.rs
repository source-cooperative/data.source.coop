//! The main proxy handler that ties together resolution and backend forwarding.
//!
//! [`ProxyHandler`] is generic over the runtime's backend and request resolver.
//! GET/HEAD operations use `send_streaming` for S3 backends (avoiding double
//! stream conversion) and `object_store` for non-S3 backends. Multipart
//! operations use raw signed HTTP requests.

use crate::backend::{hash_payload, ProxyBackend, S3RequestSigner, UNSIGNED_PAYLOAD};
use crate::error::ProxyError;
use crate::resolver::{ListRewrite, ResolvedAction, RequestResolver};
use crate::response_body::ProxyResponseBody;
use crate::s3::response::{
    ErrorResponse, ListBucketResult, ListCommonPrefix, ListContents,
};
use crate::types::{BucketConfig, S3Operation};
use bytes::Bytes;
use http::{HeaderMap, Method};
use object_store::{GetOptions, GetRange, ObjectStore, PutPayload};
use url::Url;
use uuid::Uuid;

/// The core proxy handler, generic over runtime primitives.
///
/// # Type Parameters
///
/// - `B`: The runtime's backend for object store creation and raw HTTP
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

    /// Handle an incoming S3 request.
    ///
    /// This is the main entry point. It:
    /// 1. Resolves the request via the resolver (parse, auth, authorize)
    /// 2. Forwards the request to the backing store or returns a synthetic response
    pub async fn handle_request(
        &self,
        method: Method,
        path: &str,
        query: Option<&str>,
        headers: &HeaderMap,
        body: Bytes,
    ) -> ProxyResult<B::NativeBody> {
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
        body: Bytes,
    ) -> Result<ProxyResult<B::NativeBody>, ProxyError> {
        let action = self.resolver.resolve(&method, path, query, headers).await?;

        match action {
            ResolvedAction::Response {
                status,
                headers: resp_headers,
                body: resp_body,
            } => Ok(ProxyResult {
                status,
                headers: resp_headers,
                body: ProxyResponseBody::from_bytes(resp_body),
            }),
            ResolvedAction::Proxy {
                operation,
                bucket_config,
                list_rewrite,
            } => {
                self.forward_to_backend(
                    &method,
                    &operation,
                    &bucket_config,
                    headers,
                    body,
                    list_rewrite.as_ref(),
                )
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
        body: Bytes,
        list_rewrite: Option<&ListRewrite>,
    ) -> Result<ProxyResult<B::NativeBody>, ProxyError> {
        match operation {
            S3Operation::GetObject { key, .. } => {
                self.handle_get(bucket_config, key, original_headers).await
            }
            S3Operation::HeadObject { key, .. } => {
                self.handle_head(bucket_config, key, original_headers).await
            }
            S3Operation::PutObject { key, .. } => {
                self.handle_put(bucket_config, key, body).await
            }
            S3Operation::DeleteObject { key, .. } => {
                self.handle_delete(bucket_config, key).await
            }
            S3Operation::ListBucket { raw_query, .. } => {
                self.handle_list(bucket_config, raw_query.as_deref(), list_rewrite)
                    .await
            }
            // Multipart operations go through raw signed HTTP (S3 only)
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
                self.handle_multipart(method, operation, bucket_config, original_headers, body)
                    .await
            }
            _ => Err(ProxyError::Internal("unexpected operation".into())),
        }
    }

    /// GET — uses `send_streaming` for S3 backends (zero-copy native body),
    /// falls back to `object_store` for non-S3 backends.
    async fn handle_get(
        &self,
        config: &BucketConfig,
        key: &str,
        headers: &HeaderMap,
    ) -> Result<ProxyResult<B::NativeBody>, ProxyError> {
        if config.supports_s3_multipart() {
            return self.handle_get_s3(config, key, headers).await;
        }
        self.handle_get_object_store(config, key, headers).await
    }

    /// GET for S3 backends via `send_streaming` — the response body stays in
    /// the runtime's native type, avoiding double JS/Rust stream conversion.
    async fn handle_get_s3(
        &self,
        config: &BucketConfig,
        key: &str,
        headers: &HeaderMap,
    ) -> Result<ProxyResult<B::NativeBody>, ProxyError> {
        let operation = S3Operation::GetObject {
            bucket: String::new(),
            key: key.to_string(),
        };
        let backend_url = build_backend_url(config, &operation)?;

        let mut req_headers = HeaderMap::new();

        // Forward conditional and range headers
        for header_name in &[
            "range",
            "if-match",
            "if-none-match",
            "if-modified-since",
            "if-unmodified-since",
        ] {
            if let Some(val) = headers.get(*header_name) {
                req_headers.insert(*header_name, val.clone());
            }
        }

        sign_s3_request(&Method::GET, &backend_url, &mut req_headers, config, UNSIGNED_PAYLOAD)?;

        tracing::debug!(backend_url = %backend_url, "GET via send_streaming (S3)");

        let raw = self.backend.send_streaming(Method::GET, backend_url, req_headers).await?;

        // Forward response headers from the backend
        let mut resp_headers = HeaderMap::new();
        for header_name in STREAMING_RESPONSE_HEADERS {
            if let Some(val) = raw.headers.get(*header_name) {
                resp_headers.insert(*header_name, val.clone());
            }
        }

        Ok(ProxyResult {
            status: raw.status,
            headers: resp_headers,
            body: ProxyResponseBody::Native(raw.body),
        })
    }

    /// GET for non-S3 backends via `object_store`.
    async fn handle_get_object_store(
        &self,
        config: &BucketConfig,
        key: &str,
        headers: &HeaderMap,
    ) -> Result<ProxyResult<B::NativeBody>, ProxyError> {
        let store = self.backend.create_store(config)?;
        let path = build_object_path(config, key);

        let mut opts = GetOptions::default();

        // Parse conditional headers
        if let Some(val) = headers.get("if-match").and_then(|v| v.to_str().ok()) {
            opts.if_match = Some(val.to_string());
        }
        if let Some(val) = headers.get("if-none-match").and_then(|v| v.to_str().ok()) {
            opts.if_none_match = Some(val.to_string());
        }
        if let Some(val) = headers.get("if-modified-since").and_then(|v| v.to_str().ok()) {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(val) {
                opts.if_modified_since = Some(dt.with_timezone(&chrono::Utc));
            }
        }
        if let Some(val) = headers
            .get("if-unmodified-since")
            .and_then(|v| v.to_str().ok())
        {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(val) {
                opts.if_unmodified_since = Some(dt.with_timezone(&chrono::Utc));
            }
        }

        // Parse Range header
        if let Some(range_val) = headers.get("range").and_then(|v| v.to_str().ok()) {
            if let Some(range) = parse_range_header(range_val) {
                opts.range = Some(range);
            }
        }

        tracing::debug!(path = %path, "GET via object_store");

        let result = store
            .get_opts(&path, opts)
            .await
            .map_err(ProxyError::from_object_store_error)?;

        // Build response headers from metadata
        let mut resp_headers = HeaderMap::new();
        if let Some(etag) = &result.meta.e_tag {
            resp_headers.insert("etag", etag.parse().unwrap());
        }
        resp_headers.insert(
            "last-modified",
            result
                .meta
                .last_modified
                .format("%a, %d %b %Y %H:%M:%S GMT")
                .to_string()
                .parse()
                .unwrap(),
        );
        let content_length = result.range.end - result.range.start;
        resp_headers.insert("content-length", content_length.to_string().parse().unwrap());
        resp_headers.insert("accept-ranges", "bytes".parse().unwrap());

        // If this is a range response, set 206 + Content-Range
        let status = if result.range.start > 0
            || result.range.end < result.meta.size
        {
            resp_headers.insert(
                "content-range",
                format!(
                    "bytes {}-{}/{}",
                    result.range.start,
                    result.range.end.saturating_sub(1),
                    result.meta.size
                )
                .parse()
                .unwrap(),
            );
            206
        } else {
            200
        };

        let stream = result.into_stream();

        Ok(ProxyResult {
            status,
            headers: resp_headers,
            body: ProxyResponseBody::Stream(stream),
        })
    }

    /// HEAD — uses `send_streaming` for S3 backends (richer headers from backend),
    /// falls back to `object_store` for non-S3 backends.
    async fn handle_head(
        &self,
        config: &BucketConfig,
        key: &str,
        headers: &HeaderMap,
    ) -> Result<ProxyResult<B::NativeBody>, ProxyError> {
        if config.supports_s3_multipart() {
            return self.handle_head_s3(config, key, headers).await;
        }
        self.handle_head_object_store(config, key).await
    }

    /// HEAD for S3 backends via `send_streaming`.
    async fn handle_head_s3(
        &self,
        config: &BucketConfig,
        key: &str,
        headers: &HeaderMap,
    ) -> Result<ProxyResult<B::NativeBody>, ProxyError> {
        let operation = S3Operation::HeadObject {
            bucket: String::new(),
            key: key.to_string(),
        };
        let backend_url = build_backend_url(config, &operation)?;

        let mut req_headers = HeaderMap::new();

        // Forward conditional headers
        for header_name in &[
            "if-match",
            "if-none-match",
            "if-modified-since",
            "if-unmodified-since",
        ] {
            if let Some(val) = headers.get(*header_name) {
                req_headers.insert(*header_name, val.clone());
            }
        }

        sign_s3_request(&Method::HEAD, &backend_url, &mut req_headers, config, UNSIGNED_PAYLOAD)?;

        tracing::debug!(backend_url = %backend_url, "HEAD via send_streaming (S3)");

        let raw = self.backend.send_streaming(Method::HEAD, backend_url, req_headers).await?;

        // Forward response headers from the backend
        let mut resp_headers = HeaderMap::new();
        for header_name in STREAMING_RESPONSE_HEADERS {
            if let Some(val) = raw.headers.get(*header_name) {
                resp_headers.insert(*header_name, val.clone());
            }
        }

        Ok(ProxyResult {
            status: raw.status,
            headers: resp_headers,
            body: ProxyResponseBody::Empty,
        })
    }

    /// HEAD for non-S3 backends via `object_store`.
    async fn handle_head_object_store(
        &self,
        config: &BucketConfig,
        key: &str,
    ) -> Result<ProxyResult<B::NativeBody>, ProxyError> {
        let store = self.backend.create_store(config)?;
        let path = build_object_path(config, key);

        tracing::debug!(path = %path, "HEAD via object_store");

        let meta = store
            .head(&path)
            .await
            .map_err(ProxyError::from_object_store_error)?;

        let mut resp_headers = HeaderMap::new();
        if let Some(etag) = &meta.e_tag {
            resp_headers.insert("etag", etag.parse().unwrap());
        }
        resp_headers.insert(
            "last-modified",
            meta.last_modified
                .format("%a, %d %b %Y %H:%M:%S GMT")
                .to_string()
                .parse()
                .unwrap(),
        );
        resp_headers.insert("content-length", meta.size.to_string().parse().unwrap());
        resp_headers.insert("accept-ranges", "bytes".parse().unwrap());

        Ok(ProxyResult {
            status: 200,
            headers: resp_headers,
            body: ProxyResponseBody::Empty,
        })
    }

    /// PUT via object_store
    async fn handle_put(
        &self,
        config: &BucketConfig,
        key: &str,
        body: Bytes,
    ) -> Result<ProxyResult<B::NativeBody>, ProxyError> {
        let store = self.backend.create_store(config)?;
        let path = build_object_path(config, key);

        tracing::debug!(path = %path, body_len = body.len(), "PUT via object_store");

        let payload = PutPayload::from(body);
        let result = store
            .put(&path, payload)
            .await
            .map_err(ProxyError::from_object_store_error)?;

        let mut resp_headers = HeaderMap::new();
        if let Some(etag) = &result.e_tag {
            resp_headers.insert("etag", etag.parse().unwrap());
        }

        Ok(ProxyResult {
            status: 200,
            headers: resp_headers,
            body: ProxyResponseBody::Empty,
        })
    }

    /// DELETE via object_store
    async fn handle_delete(
        &self,
        config: &BucketConfig,
        key: &str,
    ) -> Result<ProxyResult<B::NativeBody>, ProxyError> {
        let store = self.backend.create_store(config)?;
        let path = build_object_path(config, key);

        tracing::debug!(path = %path, "DELETE via object_store");

        store
            .delete(&path)
            .await
            .map_err(ProxyError::from_object_store_error)?;

        Ok(ProxyResult {
            status: 204,
            headers: HeaderMap::new(),
            body: ProxyResponseBody::Empty,
        })
    }

    /// LIST via object_store
    async fn handle_list(
        &self,
        config: &BucketConfig,
        raw_query: Option<&str>,
        list_rewrite: Option<&ListRewrite>,
    ) -> Result<ProxyResult<B::NativeBody>, ProxyError> {
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

    /// Multipart operations via raw signed HTTP
    async fn handle_multipart(
        &self,
        method: &Method,
        operation: &S3Operation,
        bucket_config: &BucketConfig,
        original_headers: &HeaderMap,
        body: Bytes,
    ) -> Result<ProxyResult<B::NativeBody>, ProxyError> {
        let backend_url = build_backend_url(bucket_config, operation)?;

        tracing::debug!(backend_url = %backend_url, "multipart via raw HTTP");

        let mut headers = HeaderMap::new();

        // Forward relevant headers
        for header_name in &[
            "content-type",
            "content-length",
            "content-md5",
        ] {
            if let Some(val) = original_headers.get(*header_name) {
                headers.insert(*header_name, val.clone());
            }
        }

        let payload_hash = if body.is_empty() {
            UNSIGNED_PAYLOAD.to_string()
        } else {
            hash_payload(&body)
        };

        sign_s3_request(method, &backend_url, &mut headers, bucket_config, &payload_hash)?;

        let raw_resp = self
            .backend
            .send_raw(method.clone(), backend_url, headers, body)
            .await?;

        tracing::debug!(status = raw_resp.status, "multipart backend response");

        Ok(ProxyResult {
            status: raw_resp.status,
            headers: raw_resp.headers,
            body: ProxyResponseBody::from_bytes(raw_resp.body),
        })
    }
}

/// The result of handling a proxy request, generic over the native body type.
pub struct ProxyResult<N = ()> {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: ProxyResponseBody<N>,
}

/// Headers to forward from backend streaming responses.
const STREAMING_RESPONSE_HEADERS: &[&str] = &[
    "content-type",
    "content-length",
    "content-range",
    "etag",
    "last-modified",
    "accept-ranges",
    "content-encoding",
    "content-disposition",
    "cache-control",
];

fn error_response<N>(err: &ProxyError, resource: &str, request_id: &str) -> ProxyResult<N> {
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
/// If credentials are configured (`access_key_id` + `secret_access_key`),
/// applies SigV4 signing. Otherwise, just sets the Host header.
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

/// Parse an HTTP Range header value into an object_store GetRange.
fn parse_range_header(value: &str) -> Option<GetRange> {
    let range_str = value.strip_prefix("bytes=")?;

    if let Some(suffix) = range_str.strip_prefix('-') {
        // bytes=-N (suffix)
        let n: u64 = suffix.parse().ok()?;
        return Some(GetRange::Suffix(n));
    }

    let (start_str, end_str) = range_str.split_once('-')?;
    let start: u64 = start_str.parse().ok()?;

    if end_str.is_empty() {
        // bytes=N- (offset to end)
        Some(GetRange::Offset(start))
    } else {
        // bytes=N-M (bounded, HTTP is inclusive, object_store is exclusive)
        let end: u64 = end_str.parse().ok()?;
        Some(GetRange::Bounded(start..end + 1))
    }
}

/// Build the backend URL for an S3 operation.
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
