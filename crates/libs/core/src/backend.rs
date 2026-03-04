//! Backend abstraction for proxying requests to backing object stores.
//!
//! [`ProxyBackend`] is the main trait runtimes implement. It provides three
//! capabilities:
//!
//! 1. **`create_paginated_store()`** — build a `PaginatedListStore` for LIST
//!    operations with backend-side pagination.
//! 2. **`create_signer()`** — build a `Signer` for generating presigned URLs
//!    for GET, HEAD, PUT, DELETE operations.
//! 3. **`send_raw()`** — send a pre-signed HTTP request for operations not
//!    covered by `ObjectStore` (multipart uploads).
//!
//! [`S3RequestSigner`] is retained for signing multipart requests.
//! [`build_paginated_list_store`] and [`build_signer`] dispatch on
//! `BucketConfig::backend_type` to build the appropriate provider.
//! [`build_signer`] uses `object_store`'s built-in signer for authenticated
//! backends, and [`UnsignedUrlSigner`] for anonymous backends (avoiding
//! `Instant::now()` which panics on WASM).

use crate::error::ProxyError;
use crate::maybe_send::{MaybeSend, MaybeSync};
use crate::types::{BackendType, BucketConfig};
use bytes::Bytes;
use http::HeaderMap;
use object_store::aws::AmazonS3Builder;
use object_store::list::PaginatedListStore;
use object_store::signer::Signer;
use object_store::ObjectStore;
use std::future::Future;
use std::sync::Arc;

#[cfg(feature = "azure")]
use object_store::azure::MicrosoftAzureBuilder;
#[cfg(feature = "gcp")]
use object_store::gcp::GoogleCloudStorageBuilder;

/// Trait for runtime-specific backend operations.
///
/// Each runtime provides its own implementation:
/// - Server runtime: uses `reqwest` for raw HTTP, default `object_store` HTTP connector
/// - Worker runtime: uses `web_sys::fetch` for raw HTTP, custom `FetchConnector` for `object_store`
pub trait ProxyBackend: Clone + MaybeSend + MaybeSync + 'static {
    /// Create a [`PaginatedListStore`] for the given bucket configuration.
    ///
    /// Used for LIST operations with backend-side pagination via
    /// [`PaginatedListStore::list_paginated`], avoiding loading all results
    /// into memory.
    fn create_paginated_store(
        &self,
        config: &BucketConfig,
    ) -> Result<Box<dyn PaginatedListStore>, ProxyError>;

    /// Create a `Signer` for generating presigned URLs.
    ///
    /// Used for GET, HEAD, PUT, DELETE operations. The handler generates
    /// a presigned URL and the runtime executes the request with its
    /// native HTTP client, enabling zero-copy streaming.
    fn create_signer(&self, config: &BucketConfig) -> Result<Arc<dyn Signer>, ProxyError>;

    /// Send a raw HTTP request (used for multipart operations that
    /// `ObjectStore` doesn't expose at the right abstraction level).
    fn send_raw(
        &self,
        method: http::Method,
        url: String,
        headers: HeaderMap,
        body: Bytes,
    ) -> impl Future<Output = Result<RawResponse, ProxyError>> + MaybeSend;
}

/// Response from a raw HTTP request to a backend.
pub struct RawResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Bytes,
}

/// Wrapper around provider-specific `object_store` builders.
///
/// Runtimes use [`build_object_store`] and inject their HTTP connector
/// via a closure that receives this enum.
pub enum StoreBuilder {
    S3(AmazonS3Builder),
    #[cfg(feature = "azure")]
    Azure(MicrosoftAzureBuilder),
    #[cfg(feature = "gcp")]
    Gcs(GoogleCloudStorageBuilder),
}

impl StoreBuilder {
    /// Build the final `ObjectStore`.
    pub fn build(self) -> Result<Arc<dyn ObjectStore>, ProxyError> {
        match self {
            StoreBuilder::S3(b) => Ok(Arc::new(b.build().map_err(|e| {
                ProxyError::ConfigError(format!("failed to build S3 store: {}", e))
            })?)),
            #[cfg(feature = "azure")]
            StoreBuilder::Azure(b) => Ok(Arc::new(b.build().map_err(|e| {
                ProxyError::ConfigError(format!("failed to build Azure store: {}", e))
            })?)),
            #[cfg(feature = "gcp")]
            StoreBuilder::Gcs(b) => Ok(Arc::new(b.build().map_err(|e| {
                ProxyError::ConfigError(format!("failed to build GCS store: {}", e))
            })?)),
        }
    }

    /// Build a `PaginatedListStore` for backend-side paginated listing.
    pub fn build_paginated(self) -> Result<Box<dyn PaginatedListStore>, ProxyError> {
        match self {
            StoreBuilder::S3(b) => Ok(Box::new(b.build().map_err(|e| {
                ProxyError::ConfigError(format!("failed to build S3 paginated store: {}", e))
            })?)),
            #[cfg(feature = "azure")]
            StoreBuilder::Azure(b) => Ok(Box::new(b.build().map_err(|e| {
                ProxyError::ConfigError(format!("failed to build Azure paginated store: {}", e))
            })?)),
            #[cfg(feature = "gcp")]
            StoreBuilder::Gcs(b) => Ok(Box::new(b.build().map_err(|e| {
                ProxyError::ConfigError(format!("failed to build GCS paginated store: {}", e))
            })?)),
        }
    }

    /// Build a `Signer` for presigned URL generation.
    pub fn build_signer(self) -> Result<Arc<dyn Signer>, ProxyError> {
        match self {
            StoreBuilder::S3(b) => Ok(Arc::new(b.build().map_err(|e| {
                ProxyError::ConfigError(format!("failed to build S3 signer: {}", e))
            })?)),
            #[cfg(feature = "azure")]
            StoreBuilder::Azure(b) => Ok(Arc::new(b.build().map_err(|e| {
                ProxyError::ConfigError(format!("failed to build Azure signer: {}", e))
            })?)),
            #[cfg(feature = "gcp")]
            StoreBuilder::Gcs(b) => Ok(Arc::new(b.build().map_err(|e| {
                ProxyError::ConfigError(format!("failed to build GCS signer: {}", e))
            })?)),
        }
    }
}

/// Create a [`StoreBuilder`] from a [`BucketConfig`], dispatching on `backend_type`.
fn create_builder(config: &BucketConfig) -> Result<StoreBuilder, ProxyError> {
    let backend_type = config.parsed_backend_type().ok_or_else(|| {
        ProxyError::ConfigError(format!(
            "unsupported backend_type: '{}'",
            config.backend_type
        ))
    })?;

    match backend_type {
        BackendType::S3 => {
            let mut b = AmazonS3Builder::new();
            for (k, v) in &config.backend_options {
                if let Ok(key) = k.parse() {
                    b = b.with_config(key, v);
                }
            }
            Ok(StoreBuilder::S3(b))
        }
        #[cfg(feature = "azure")]
        BackendType::Azure => {
            let mut b = MicrosoftAzureBuilder::new();
            for (k, v) in &config.backend_options {
                if let Ok(key) = k.parse() {
                    b = b.with_config(key, v);
                }
            }
            Ok(StoreBuilder::Azure(b))
        }
        #[cfg(not(feature = "azure"))]
        BackendType::Azure => Err(ProxyError::ConfigError(
            "Azure backend support not enabled (requires 'azure' feature)".into(),
        )),
        #[cfg(feature = "gcp")]
        BackendType::Gcs => {
            let mut b = GoogleCloudStorageBuilder::new();
            for (k, v) in &config.backend_options {
                if let Ok(key) = k.parse() {
                    b = b.with_config(key, v);
                }
            }
            Ok(StoreBuilder::Gcs(b))
        }
        #[cfg(not(feature = "gcp"))]
        BackendType::Gcs => Err(ProxyError::ConfigError(
            "GCS backend support not enabled (requires 'gcp' feature)".into(),
        )),
    }
}

/// Build an `ObjectStore` from a [`BucketConfig`], dispatching on `backend_type`.
///
/// The `configure` closure lets each runtime inject its HTTP connector:
/// - Server runtime passes `|b| b` (default connector)
/// - CF Workers passes `|b| match b { StoreBuilder::S3(s) => StoreBuilder::S3(s.with_http_connector(FetchConnector)), .. }`
pub fn build_object_store<F>(
    config: &BucketConfig,
    configure: F,
) -> Result<Arc<dyn ObjectStore>, ProxyError>
where
    F: FnOnce(StoreBuilder) -> StoreBuilder,
{
    configure(create_builder(config)?).build()
}

/// Build a [`PaginatedListStore`] from a [`BucketConfig`], dispatching on `backend_type`.
///
/// Like [`build_object_store`], accepts a configure closure for HTTP connector injection.
pub fn build_paginated_list_store<F>(
    config: &BucketConfig,
    configure: F,
) -> Result<Box<dyn PaginatedListStore>, ProxyError>
where
    F: FnOnce(StoreBuilder) -> StoreBuilder,
{
    configure(create_builder(config)?).build_paginated()
}

/// Build a [`Signer`] from a [`BucketConfig`], dispatching on `backend_type`.
///
/// For backends with credentials, uses `object_store`'s built-in signer
/// (WASM-safe because `StaticCredentialProvider` bypasses `Instant::now()`).
/// For anonymous backends (no credentials), returns [`UnsignedUrlSigner`]
/// which constructs plain URLs without auth parameters, avoiding the
/// `InstanceCredentialProvider` → `Instant::now()` panic on WASM.
pub fn build_signer(config: &BucketConfig) -> Result<Arc<dyn Signer>, ProxyError> {
    let backend_type = config.parsed_backend_type().ok_or_else(|| {
        ProxyError::ConfigError(format!(
            "unsupported backend_type: '{}'",
            config.backend_type
        ))
    })?;

    // Check for credentials — if absent, return unsigned signer to avoid
    // InstanceCredentialProvider which uses Instant::now() (panics on WASM).
    let has_creds = !config.option("access_key_id").unwrap_or("").is_empty()
        && !config.option("secret_access_key").unwrap_or("").is_empty();

    if !has_creds {
        return Ok(Arc::new(UnsignedUrlSigner::from_config(config)?));
    }

    match backend_type {
        BackendType::S3 => create_builder(config)?.build_signer(),
        #[cfg(feature = "azure")]
        BackendType::Azure => create_builder(config)?.build_signer(),
        #[cfg(not(feature = "azure"))]
        BackendType::Azure => Err(ProxyError::ConfigError(
            "Azure backend support not enabled (requires 'azure' feature)".into(),
        )),
        #[cfg(feature = "gcp")]
        BackendType::Gcs => create_builder(config)?.build_signer(),
        #[cfg(not(feature = "gcp"))]
        BackendType::Gcs => Err(ProxyError::ConfigError(
            "GCS backend support not enabled (requires 'gcp' feature)".into(),
        )),
    }
}

/// Helper to build a signed URL + headers for an outbound request to S3.
///
/// Used for multipart operations (CreateMultipartUpload, UploadPart,
/// CompleteMultipartUpload, AbortMultipartUpload) that go through raw HTTP.
pub struct S3RequestSigner {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: String,
    pub service: String,
    pub session_token: Option<String>,
}

impl S3RequestSigner {
    pub fn new(
        access_key_id: String,
        secret_access_key: String,
        region: String,
        session_token: Option<String>,
    ) -> Self {
        Self {
            access_key_id,
            secret_access_key,
            region,
            service: "s3".to_string(),
            session_token,
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

        if let Some(token) = &self.session_token {
            headers.insert("x-amz-security-token", token.parse().unwrap());
        }

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

        let mut signed_header_names: Vec<&str> = headers.keys().map(|k| k.as_str()).collect();
        signed_header_names.sort();

        let canonical_headers: String = signed_header_names
            .iter()
            .map(|k| {
                let v = headers.get(*k).unwrap().to_str().unwrap_or("").trim();
                format!("{}:{}\n", k, v)
            })
            .collect();

        let signed_headers = signed_header_names.join(";");

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method,
            canonical_uri,
            canonical_querystring,
            canonical_headers,
            signed_headers,
            payload_hash
        );

        // String to sign
        let credential_scope = format!(
            "{}/{}/{}/aws4_request",
            date_stamp, self.region, self.service
        );

        use sha2::Digest;
        let canonical_request_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));

        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            amz_date, credential_scope, canonical_request_hash
        );

        // Signing key
        type HmacSha256 = Hmac<Sha256>;

        let mut mac =
            HmacSha256::new_from_slice(format!("AWS4{}", self.secret_access_key).as_bytes())
                .map_err(|e| ProxyError::Internal(e.to_string()))?;
        mac.update(date_stamp.as_bytes());
        let k_date = mac.finalize().into_bytes();

        let mut mac =
            HmacSha256::new_from_slice(&k_date).map_err(|e| ProxyError::Internal(e.to_string()))?;
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

/// Signer for anonymous/credential-less backends.
///
/// Returns unsigned URLs — no auth query params, no time calls. This avoids
/// the `InstanceCredentialProvider` → `TokenCache` → `Instant::now()` path
/// in `object_store` which panics on `wasm32-unknown-unknown`.
#[derive(Debug)]
struct UnsignedUrlSigner {
    endpoint: String,
    bucket: String,
}

impl UnsignedUrlSigner {
    fn from_config(config: &BucketConfig) -> Result<Self, ProxyError> {
        let endpoint = config
            .option("endpoint")
            .unwrap_or("https://s3.amazonaws.com");
        let bucket = config.option("bucket_name").unwrap_or("");
        Ok(Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            bucket: bucket.to_string(),
        })
    }
}

#[async_trait::async_trait]
impl Signer for UnsignedUrlSigner {
    async fn signed_url(
        &self,
        _method: http::Method,
        path: &object_store::path::Path,
        _expires_in: std::time::Duration,
    ) -> object_store::Result<url::Url> {
        let key = path.as_ref();
        let url_str = if self.bucket.is_empty() {
            if key.is_empty() {
                format!("{}/", self.endpoint)
            } else {
                format!("{}/{}", self.endpoint, key)
            }
        } else if key.is_empty() {
            format!("{}/{}", self.endpoint, self.bucket)
        } else {
            format!("{}/{}/{}", self.endpoint, self.bucket, key)
        };
        url::Url::parse(&url_str).map_err(|e| object_store::Error::Generic {
            store: "UnsignedUrlSigner",
            source: Box::new(e),
        })
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
