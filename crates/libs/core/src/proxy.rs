//! The main proxy handler that ties together resolution and backend forwarding.
//!
//! [`ProxyHandler`] is generic over the runtime's backend and request resolver.
//! GET/HEAD/PUT/LIST operations go through `object_store`; multipart operations
//! use raw signed HTTP requests.

use crate::backend::{hash_payload, ProxyBackend, S3RequestSigner, UNSIGNED_PAYLOAD};
use crate::error::ProxyError;
use crate::resolver::{ListRewrite, ResolvedAction, RequestResolver};
use crate::response_body::ProxyResponseBody;
use crate::s3::response::ErrorResponse;
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
    ) -> ProxyResult {
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
    ) -> Result<ProxyResult, ProxyError> {
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
    ) -> Result<ProxyResult, ProxyError> {
        match operation {
            S3Operation::GetObject { key, .. } => {
                self.handle_get(bucket_config, key, original_headers).await
            }
            S3Operation::HeadObject { key, .. } => {
                self.handle_head(bucket_config, key).await
            }
            S3Operation::PutObject { key, .. } => {
                self.handle_put(bucket_config, key, body).await
            }
            S3Operation::ListBucket { raw_query, .. } => {
                self.handle_list(bucket_config, raw_query.as_deref(), list_rewrite)
                    .await
            }
            // Multipart operations go through raw signed HTTP
            S3Operation::CreateMultipartUpload { .. }
            | S3Operation::UploadPart { .. }
            | S3Operation::CompleteMultipartUpload { .. }
            | S3Operation::AbortMultipartUpload { .. } => {
                self.handle_multipart(method, operation, bucket_config, original_headers, body)
                    .await
            }
            _ => Err(ProxyError::Internal("unexpected operation".into())),
        }
    }

    /// GET via object_store
    async fn handle_get(
        &self,
        config: &BucketConfig,
        key: &str,
        headers: &HeaderMap,
    ) -> Result<ProxyResult, ProxyError> {
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

    /// HEAD via object_store
    async fn handle_head(
        &self,
        config: &BucketConfig,
        key: &str,
    ) -> Result<ProxyResult, ProxyError> {
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
    ) -> Result<ProxyResult, ProxyError> {
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

    /// Multipart operations via raw signed HTTP
    async fn handle_multipart(
        &self,
        method: &Method,
        operation: &S3Operation,
        bucket_config: &BucketConfig,
        original_headers: &HeaderMap,
        body: Bytes,
    ) -> Result<ProxyResult, ProxyError> {
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

        // Sign the request if credentials are configured
        let has_credentials = !bucket_config.backend_access_key_id.is_empty()
            && !bucket_config.backend_secret_access_key.is_empty();

        let parsed_url = Url::parse(&backend_url)
            .map_err(|e| ProxyError::Internal(format!("invalid backend URL: {}", e)))?;

        let payload_hash = if body.is_empty() {
            UNSIGNED_PAYLOAD.to_string()
        } else {
            hash_payload(&body)
        };

        if has_credentials {
            let signer = S3RequestSigner::new(
                bucket_config.backend_access_key_id.clone(),
                bucket_config.backend_secret_access_key.clone(),
                bucket_config.backend_region.clone(),
            );
            signer.sign_request(method, &parsed_url, &mut headers, &payload_hash)?;
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

/// The result of handling a proxy request.
pub struct ProxyResult {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: ProxyResponseBody,
}

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

    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Name>{}</Name>\
         <Prefix>{}</Prefix>\
         <Delimiter>{}</Delimiter>\
         <MaxKeys>1000</MaxKeys>\
         <IsTruncated>false</IsTruncated>\
         <KeyCount>{}</KeyCount>",
        bucket_name,
        client_prefix,
        delimiter,
        list_result.objects.len() + list_result.common_prefixes.len()
    );

    for obj in &list_result.objects {
        let raw_key = obj.location.to_string();
        let key = rewrite_key(&raw_key, &strip_prefix, list_rewrite);

        let last_modified = obj.last_modified.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let etag = obj.e_tag.as_deref().unwrap_or("\"\"");

        xml.push_str(&format!(
            "<Contents>\
             <Key>{}</Key>\
             <LastModified>{}</LastModified>\
             <ETag>{}</ETag>\
             <Size>{}</Size>\
             <StorageClass>STANDARD</StorageClass>\
             </Contents>",
            key, last_modified, etag, obj.size
        ));
    }

    for prefix_path in &list_result.common_prefixes {
        let raw_prefix = format!("{}/", prefix_path);
        let prefix = rewrite_key(&raw_prefix, &strip_prefix, list_rewrite);

        xml.push_str(&format!(
            "<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>",
            prefix
        ));
    }

    xml.push_str("</ListBucketResult>");
    xml
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

fn build_backend_url(
    config: &BucketConfig,
    operation: &S3Operation,
) -> Result<String, ProxyError> {
    let base = config.backend_endpoint.trim_end_matches('/');
    let bucket = &config.backend_bucket;
    let bucket_is_empty = bucket.is_empty();

    let key = match operation {
        S3Operation::CreateMultipartUpload { key, .. }
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
        _ => return Err(ProxyError::Internal("unexpected operation for multipart URL".into())),
    };

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
