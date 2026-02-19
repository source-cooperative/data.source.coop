//! Backend client abstraction for making signed requests to backing object stores.

use crate::error::ProxyError;
use crate::maybe_send::{MaybeSend, MaybeSync};
use crate::stream::BodyStream;
use http::HeaderMap;
use std::future::Future;

/// A fully prepared request to send to a backend object store.
#[derive(Debug)]
pub struct BackendRequest<B> {
    pub method: http::Method,
    pub url: String,
    pub headers: HeaderMap,
    pub body: B,
}

/// The response from a backend object store.
pub struct BackendResponse<B> {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: B,
}

/// Trait for making outbound HTTP requests to backing object stores.
///
/// Each runtime provides its own implementation:
/// - Server runtime: uses `hyper` client with native async streaming
/// - Worker runtime: uses the Fetch API, keeping JS `ReadableStream` intact
///
/// The body type `B` is the runtime's native stream type. This ensures
/// zero-copy passthrough: the proxy never materializes the full response
/// body in memory.
pub trait BackendClient: MaybeSend + MaybeSync + 'static {
    type Body: BodyStream;

    fn send_request(
        &self,
        request: BackendRequest<Self::Body>,
    ) -> impl Future<Output = Result<BackendResponse<Self::Body>, ProxyError>> + MaybeSend;
}

/// Helper to build a signed URL + headers for an outbound request to S3.
pub struct S3RequestSigner {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: String,
    pub service: String,
}

impl S3RequestSigner {
    pub fn new(
        access_key_id: String,
        secret_access_key: String,
        region: String,
    ) -> Self {
        Self {
            access_key_id,
            secret_access_key,
            region,
            service: "s3".to_string(),
        }
    }

    /// Sign an outbound request using AWS SigV4.
    ///
    /// This adds Authorization, x-amz-date, x-amz-content-sha256, and Host
    /// headers to the provided header map.
    pub fn sign_request(
        &self,
        method: &http::Method,
        url: &url::Url,
        headers: &mut HeaderMap,
        payload_hash: &str,
    ) -> Result<(), ProxyError> {
        use chrono::Utc;
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let now = Utc::now();
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

        // Set required headers
        headers.insert("x-amz-date", amz_date.parse().unwrap());
        headers.insert("x-amz-content-sha256", payload_hash.parse().unwrap());

        let host = url
            .host_str()
            .ok_or_else(|| ProxyError::Internal("no host in URL".into()))?;
        let host_header = if let Some(port) = url.port() {
            format!("{}:{}", host, port)
        } else {
            host.to_string()
        };
        headers.insert("host", host_header.parse().unwrap());

        // Canonical request
        let canonical_uri = url.path();
        let canonical_querystring = url.query().unwrap_or("");

        let mut signed_header_names: Vec<&str> = headers
            .keys()
            .map(|k| k.as_str())
            .collect();
        signed_header_names.sort();

        let canonical_headers: String = signed_header_names
            .iter()
            .map(|k| {
                let v = headers
                    .get(*k)
                    .unwrap()
                    .to_str()
                    .unwrap_or("")
                    .trim();
                format!("{}:{}\n", k, v)
            })
            .collect();

        let signed_headers = signed_header_names.join(";");

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method, canonical_uri, canonical_querystring, canonical_headers, signed_headers, payload_hash
        );

        // String to sign
        let credential_scope = format!("{}/{}/{}/aws4_request", date_stamp, self.region, self.service);

        use sha2::Digest;
        let canonical_request_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));

        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            amz_date, credential_scope, canonical_request_hash
        );

        // Signing key
        type HmacSha256 = Hmac<Sha256>;

        let mut mac = HmacSha256::new_from_slice(
            format!("AWS4{}", self.secret_access_key).as_bytes(),
        )
        .map_err(|e| ProxyError::Internal(e.to_string()))?;
        mac.update(date_stamp.as_bytes());
        let k_date = mac.finalize().into_bytes();

        let mut mac = HmacSha256::new_from_slice(&k_date)
            .map_err(|e| ProxyError::Internal(e.to_string()))?;
        mac.update(self.region.as_bytes());
        let k_region = mac.finalize().into_bytes();

        let mut mac = HmacSha256::new_from_slice(&k_region)
            .map_err(|e| ProxyError::Internal(e.to_string()))?;
        mac.update(self.service.as_bytes());
        let k_service = mac.finalize().into_bytes();

        let mut mac = HmacSha256::new_from_slice(&k_service)
            .map_err(|e| ProxyError::Internal(e.to_string()))?;
        mac.update(b"aws4_request");
        let signing_key = mac.finalize().into_bytes();

        // Signature
        let mut mac = HmacSha256::new_from_slice(&signing_key)
            .map_err(|e| ProxyError::Internal(e.to_string()))?;
        mac.update(string_to_sign.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());

        // Authorization header
        let auth_header = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            self.access_key_id, credential_scope, signed_headers, signature
        );
        headers.insert("authorization", auth_header.parse().unwrap());

        Ok(())
    }
}

/// Hash a payload for SigV4. For streaming/unsigned payloads, use the
/// special sentinel value.
pub fn hash_payload(payload: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(payload))
}

/// The SigV4 sentinel for unsigned payloads (used with streaming uploads).
pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

/// The SigV4 sentinel for streaming payloads.
pub const STREAMING_PAYLOAD: &str = "STREAMING-AWS4-HMAC-SHA256-PAYLOAD";
