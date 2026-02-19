//! Parse incoming HTTP requests into typed S3 operations.

use crate::error::ProxyError;
use crate::types::S3Operation;
use http::Method;

/// Extract the bucket and key from a path-style S3 request.
///
/// Path-style: `/{bucket}/{key}`
/// Virtual-hosted-style: Host header `{bucket}.s3.example.com` with path `/{key}`
pub fn parse_s3_request(
    method: &Method,
    uri_path: &str,
    query: Option<&str>,
    _headers: &http::HeaderMap,
    host_style: HostStyle,
) -> Result<S3Operation, ProxyError> {
    // Check for STS actions in query params
    if let Some(q) = query {
        let params: Vec<(String, String)> = url::form_urlencoded::parse(q.as_bytes())
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        let action = params.iter().find(|(k, _)| k == "Action");
        if let Some((_, action_value)) = action {
            if action_value == "AssumeRoleWithWebIdentity" {
                return parse_sts_request(&params);
            }
        }
    }

    // GET / with path-style → ListBuckets (no bucket in path)
    if matches!(host_style, HostStyle::Path) && uri_path.trim_start_matches('/').is_empty() {
        if *method == Method::GET {
            return Ok(S3Operation::ListBuckets);
        }
        return Err(ProxyError::InvalidRequest("unsupported operation on /".into()));
    }

    let (bucket, key) = match host_style {
        HostStyle::Path => parse_path_style(uri_path)?,
        HostStyle::VirtualHosted { bucket } => (bucket, uri_path.trim_start_matches('/').to_string()),
    };

    build_s3_operation(method, bucket, key, query)
}

/// Build an [`S3Operation`] from an already-extracted bucket, key, and query.
///
/// This is used by both [`parse_s3_request`] and custom resolvers that parse
/// the path themselves (e.g., Source Cooperative).
pub fn build_s3_operation(
    method: &Method,
    bucket: String,
    key: String,
    query: Option<&str>,
) -> Result<S3Operation, ProxyError> {
    let query_params = parse_query_params(query);

    // Check for multipart upload query params
    let upload_id = query_params
        .iter()
        .find(|(k, _)| k == "uploadId")
        .map(|(_, v)| v.clone());

    let has_uploads = query_params.iter().any(|(k, _)| k == "uploads");

    match method {
        &Method::GET => {
            if key.is_empty() {
                // ListBucket — pass the raw query string through so the proxy
                // can forward all list params (prefix, delimiter, max-keys,
                // continuation-token, list-type, start-after, etc.) to the backend.
                Ok(S3Operation::ListBucket {
                    bucket,
                    raw_query: query.map(|q| q.to_string()),
                })
            } else {
                Ok(S3Operation::GetObject { bucket, key })
            }
        }
        &Method::HEAD => Ok(S3Operation::HeadObject { bucket, key }),
        &Method::PUT => {
            if let Some(upload_id) = upload_id {
                let part_number = query_params
                    .iter()
                    .find(|(k, _)| k == "partNumber")
                    .and_then(|(_, v)| v.parse().ok())
                    .ok_or_else(|| ProxyError::InvalidRequest("missing partNumber".into()))?;

                Ok(S3Operation::UploadPart {
                    bucket,
                    key,
                    upload_id,
                    part_number,
                })
            } else {
                Ok(S3Operation::PutObject { bucket, key })
            }
        }
        &Method::POST => {
            if has_uploads {
                Ok(S3Operation::CreateMultipartUpload { bucket, key })
            } else if let Some(upload_id) = upload_id {
                Ok(S3Operation::CompleteMultipartUpload {
                    bucket,
                    key,
                    upload_id,
                })
            } else {
                Err(ProxyError::InvalidRequest(
                    "unsupported POST operation".into(),
                ))
            }
        }
        &Method::DELETE => {
            if let Some(upload_id) = upload_id {
                Ok(S3Operation::AbortMultipartUpload {
                    bucket,
                    key,
                    upload_id,
                })
            } else {
                Err(ProxyError::InvalidRequest(
                    "unsupported DELETE operation".into(),
                ))
            }
        }
        _ => Err(ProxyError::InvalidRequest(format!(
            "unsupported method: {}",
            method
        ))),
    }
}

#[derive(Debug, Clone)]
pub enum HostStyle {
    /// Path-style: `/{bucket}/{key}`
    Path,
    /// Virtual-hosted-style: bucket extracted from Host header.
    VirtualHosted { bucket: String },
}

fn parse_path_style(path: &str) -> Result<(String, String), ProxyError> {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        return Err(ProxyError::InvalidRequest("empty path".into()));
    }

    match trimmed.split_once('/') {
        Some((bucket, key)) => Ok((bucket.to_string(), key.to_string())),
        None => Ok((trimmed.to_string(), String::new())),
    }
}

fn parse_query_params(query: Option<&str>) -> Vec<(String, String)> {
    query
        .map(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_sts_request(params: &[(String, String)]) -> Result<S3Operation, ProxyError> {
    let role_arn = params
        .iter()
        .find(|(k, _)| k == "RoleArn")
        .map(|(_, v)| v.clone())
        .ok_or_else(|| ProxyError::InvalidRequest("missing RoleArn".into()))?;

    let web_identity_token = params
        .iter()
        .find(|(k, _)| k == "WebIdentityToken")
        .map(|(_, v)| v.clone())
        .ok_or_else(|| ProxyError::InvalidRequest("missing WebIdentityToken".into()))?;

    let duration_seconds = params
        .iter()
        .find(|(k, _)| k == "DurationSeconds")
        .and_then(|(_, v)| v.parse().ok());

    Ok(S3Operation::AssumeRoleWithWebIdentity {
        role_arn,
        web_identity_token,
        duration_seconds,
    })
}
