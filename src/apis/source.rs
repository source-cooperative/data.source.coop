//! Source API client and data structures for the Source Cooperative platform.
//!
//! This module provides types and functionality for interacting with the Source API,
//! including product management, account handling, and storage backend integration.
//!
//! # Overview
//!
//! The Source Cooperative is a platform for sharing and collaborating on data products.
//! This module defines the core data structures that represent products, accounts,
//! and their associated metadata in the system.
//!
//! # Key Types
//!
//! - [`SourceProduct`] - Main product entity with metadata and configuration
//! - [`SourceProductAccount`] - Account information for product owners
//! - [`SourceProductMetadata`] - Product configuration including mirrors and roles
//! - [`SourceApi`] - API client for interacting with the Source platform
//!
//! # Examples
//!
//! ## Creating a Source API client
//!
//! ```rust
//! use source_data_proxy::apis::source::SourceApi;
//!
//! let api = SourceApi::new(
//!     "https://api.source.coop".to_string(),
//!     "your-api-key".to_string(),
//!     None
//! );
//! ```
//!
//! ## Parsing product data from JSON
//!
//! ```rust
//! use serde_json;
//! use source_data_proxy::apis::source::SourceProduct;
//!
//! let json = r#"{
//!   "product_id": "example-product",
//!   "account_id": "example-account",
//!   "title": "Example Product",
//!   "description": "An example product",
//!   "created_at": "2023-01-01T00:00:00Z",
//!   "updated_at": "2023-01-01T00:00:00Z",
//!   "visibility": "public",
//!   "disabled": false,
//!   "data_mode": "open",
//!   "featured": 0,
//!   "metadata": { ... },
//!   "account": { ... }
//! }"#;
//!
//! let product: SourceProduct = serde_json::from_str(json)?;
//! ```

use super::{Account, Api};
use crate::backends::azure::AzureRepository;
use crate::backends::common::Repository;
use crate::backends::s3::S3Repository;
use crate::utils::api::process_json_response;
use crate::utils::auth::UserIdentity;
use crate::utils::errors::BackendError;
use async_trait::async_trait;
use moka::future::Cache;
use rusoto_core::Region;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;

/// Client for interacting with the Source Cooperative API.
///
/// The `SourceApi` provides methods for managing products, accounts, and storage
/// backends. It includes built-in caching for improved performance and supports
/// both direct API calls and proxy-based requests.
///
/// # Features
///
/// - **Caching**: Built-in caching for products, data connections, and permissions
/// - **Multiple Storage Backends**: Support for S3, Azure, GCS, MinIO, and Ceph
/// - **Proxy Support**: Optional proxy configuration for network requests
/// - **Authentication**: API key-based authentication with user identity support
///
/// # Examples
///
/// ```rust
/// use source_data_proxy::apis::source::SourceApi;
///
/// let api = SourceApi::new(
///     "https://api.source.coop".to_string(),
///     "your-api-key".to_string(),
///     None // No proxy
/// );
///
/// // Get a product
/// let product = api.get_repository_record("account-id", "product-id").await?;
/// ```
#[derive(Clone)]
pub struct SourceApi {
    /// Base URL for the Source API endpoint
    pub endpoint: String,

    /// API key for authenticating requests
    api_key: String,

    /// Cache for product data to reduce API calls
    product_cache: Arc<Cache<String, SourceProduct>>,

    /// Cache for data connection configurations
    data_connection_cache: Arc<Cache<String, DataConnection>>,

    /// Cache for API key credentials
    access_key_cache: Arc<Cache<String, APIKey>>,

    /// Cache for user permissions
    permissions_cache: Arc<Cache<String, Vec<RepositoryPermission>>>,

    /// Optional proxy URL for requests
    proxy_url: Option<String>,
}

/// Repository access permissions for products.
///
/// Defines the level of access a user or account has to a specific product.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RepositoryPermission {
    /// Read-only access to the product data
    #[serde(rename = "read")]
    Read,
    /// Read and write access to the product data
    #[serde(rename = "write")]
    Write,
}

/// Product visibility levels that control who can discover and access the product.
///
/// This determines how the product appears in listings and search results.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ProductVisibility {
    /// Product is visible to everyone and appears in public listings
    #[serde(rename = "public")]
    Public,
    /// Product is not listed publicly but can be accessed with direct link
    #[serde(rename = "unlisted")]
    Unlisted,
    /// Product access is restricted to specific users or groups
    #[serde(rename = "restricted")]
    Restricted,
}

/// Data access modes that define how users can access the product's data.
///
/// This controls the business model and access patterns for the product.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ProductDataMode {
    /// Data is freely accessible to anyone
    #[serde(rename = "open")]
    Open,
    /// Data requires a subscription to access
    #[serde(rename = "subscription")]
    Subscription,
    /// Data is private and only accessible to authorized users
    #[serde(rename = "private")]
    Private,
}

/// Supported storage backend types for product data mirrors.
///
/// Each product can have multiple mirrors across different storage providers
/// for redundancy and performance optimization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StorageType {
    /// Amazon S3 compatible storage
    #[serde(rename = "s3")]
    S3,
    /// Microsoft Azure Blob Storage
    #[serde(rename = "azure")]
    Azure,
    /// Google Cloud Storage
    #[serde(rename = "gcs")]
    Gcs,
    /// MinIO object storage
    #[serde(rename = "minio")]
    Minio,
    /// Ceph distributed storage
    #[serde(rename = "ceph")]
    Ceph,
}

/// User roles that define permissions within a product.
///
/// Roles are assigned to accounts and determine what actions they can perform
/// on the product and its data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ProductRole {
    /// Full administrative access to the product
    #[serde(rename = "admin")]
    Admin,
    /// Can contribute data and manage content
    #[serde(rename = "contributor")]
    Contributor,
    /// Read-only access to the product
    #[serde(rename = "viewer")]
    Viewer,
}

/// Account types in the Source Cooperative system.
///
/// Different account types have different capabilities and metadata structures.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AccountType {
    /// Individual user account
    #[serde(rename = "individual")]
    Individual,
    /// Organization or group account
    #[serde(rename = "organization")]
    Organization,
}

/// Domain verification status for account domains.
///
/// Used to track the verification state of custom domains associated with accounts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DomainStatus {
    /// Domain has not been verified
    #[serde(rename = "unverified")]
    Unverified,
    /// Domain verification is in progress
    #[serde(rename = "pending")]
    Pending,
    /// Domain has been successfully verified
    #[serde(rename = "verified")]
    Verified,
}

/// Methods available for domain verification.
///
/// Different verification methods provide different levels of security and ease of use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum VerificationMethod {
    /// DNS-based verification using TXT records
    #[serde(rename = "dns")]
    Dns,
    /// HTML-based verification using meta tags
    #[serde(rename = "html")]
    Html,
    /// File-based verification using uploaded files
    #[serde(rename = "file")]
    File,
}

/// API key credentials for authenticating with the Source API.
///
/// Contains the access key ID and secret access key used for API authentication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct APIKey {
    /// The access key ID for API authentication
    pub access_key_id: String,
    /// The secret access key for API authentication
    pub secret_access_key: String,
}

/// Represents a product in the Source Cooperative system.
///
/// A product is the main entity that contains data and metadata, similar to a repository
/// in traditional version control systems. Products can have multiple storage mirrors
/// for redundancy and performance optimization.
///
/// # Examples
///
/// ```rust
/// use serde_json;
///
/// let json = r#"{
///   "product_id": "example-product",
///   "account_id": "example-account",
///   "title": "Example Product",
///   "description": "An example product",
///   "created_at": "2023-01-01T00:00:00Z",
///   "updated_at": "2023-01-01T00:00:00Z",
///   "visibility": "public",
///   "disabled": false,
///   "data_mode": "open",
///   "featured": 0,
///   "metadata": { ... },
///   "account": { ... }
/// }"#;
/// let product: SourceProduct = serde_json::from_str(json)?;
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProduct {
    /// Unique identifier for the product (3-40 chars, lowercase, alphanumeric with hyphens)
    pub product_id: String,

    /// ID of the account that owns this product
    pub account_id: String,

    /// Human-readable title of the product
    pub title: String,

    /// Detailed description of the product
    pub description: String,

    /// ISO 8601 timestamp when the product was created
    pub created_at: String,

    /// ISO 8601 timestamp when the product was last updated
    pub updated_at: String,

    /// Visibility level of the product
    pub visibility: ProductVisibility,

    /// Whether the product is disabled
    pub disabled: bool,

    /// Data access mode for the product
    pub data_mode: ProductDataMode,

    /// Featured status (0 = not featured, 1 = featured)
    pub featured: i32,

    /// Product metadata including mirrors, tags, and roles
    pub metadata: SourceProductMetadata,

    /// Optional account information
    pub account: Option<SourceProductAccount>,
}

/// Metadata for a product including mirrors, tags, and roles.
///
/// Contains all the configuration and organizational information for a product
/// that doesn't fit into the main product fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProductMetadata {
    /// Map of mirror names to mirror configurations
    pub mirrors: HashMap<String, SourceProductMirror>,

    /// Name of the primary mirror (key in the mirrors map)
    pub primary_mirror: String,

    /// Optional list of tags associated with the product
    pub tags: Option<Vec<String>>,

    /// Map of account IDs to their roles for this product
    pub roles: HashMap<String, SourceProductRole>,
}

/// Configuration for a storage mirror of a product.
///
/// Each product can have multiple mirrors across different storage providers
/// for redundancy and performance optimization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProductMirror {
    /// Type of storage backend used for this mirror
    pub storage_type: StorageType,

    /// ID of the data connection configuration
    pub connection_id: String,

    /// Storage prefix/path for this mirror
    pub prefix: String,

    /// Storage-specific configuration options
    pub config: SourceProductMirrorConfig,

    /// Whether this is the primary mirror for the product
    pub is_primary: bool,
}

/// Storage-specific configuration options for a mirror.
///
/// Different storage backends require different configuration parameters.
/// All fields are optional and only relevant for specific storage types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProductMirrorConfig {
    /// AWS region for S3/GCS storage
    pub region: Option<String>,

    /// Bucket name for S3/GCS storage
    pub bucket: Option<String>,

    /// Container name for Azure Blob Storage
    pub container: Option<String>,

    /// Custom endpoint URL for MinIO/Ceph storage
    pub endpoint: Option<String>,
}

/// Role assignment for product access.
///
/// Defines what role an account has for a specific product and when it was granted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProductRole {
    /// ID of the account that has this role
    pub account_id: String,

    /// The role assigned to the account
    pub role: ProductRole,

    /// ISO 8601 timestamp when the role was granted
    pub granted_at: String,

    /// ID of the account that granted this role
    pub granted_by: String,
}

/// Account information associated with a product.
///
/// Contains the account details of the product owner, including profile information,
/// contact details, and organizational metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProductAccount {
    /// Unique identifier for the account
    pub account_id: String,

    /// Type of account (individual or organization)
    #[serde(rename = "type")]
    pub account_type: AccountType,

    /// Display name of the account
    pub name: String,

    /// Identity provider ID (only for individual accounts)
    pub identity_id: Option<String>,

    /// Public metadata visible to other users
    pub metadata_public: SourceProductAccountMetadataPublic,

    /// Email addresses associated with the account
    pub emails: Option<Vec<SourceAccountEmail>>,

    /// ISO 8601 timestamp when the account was created
    pub created_at: String,

    /// ISO 8601 timestamp when the account was last updated
    pub updated_at: String,

    /// Whether the account is disabled
    pub disabled: bool,

    /// Account capability flags
    pub flags: Vec<String>,

    /// Private metadata not visible to other users
    pub metadata_private: Option<HashMap<String, serde_json::Value>>,
}

/// Domain verification information for an account.
///
/// Tracks the verification status and process for custom domains associated with accounts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountDomain {
    /// The domain name being verified
    pub domain: String,

    /// Current verification status of the domain
    pub status: DomainStatus,

    /// Method used for verification (if applicable)
    pub verification_method: Option<VerificationMethod>,

    /// Token used for verification (if applicable)
    pub verification_token: Option<String>,

    /// ISO 8601 timestamp when verification was completed
    pub verified_at: Option<String>,

    /// ISO 8601 timestamp when domain was added
    pub created_at: String,

    /// ISO 8601 timestamp when verification expires (if applicable)
    pub expires_at: Option<String>,
}

/// Email address information for an account.
///
/// Tracks email addresses associated with an account, including verification status
/// and primary email designation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceAccountEmail {
    /// The email address
    pub address: String,

    /// Whether the email address has been verified
    pub verified: bool,

    /// ISO 8601 timestamp when verification was completed
    pub verified_at: Option<String>,

    /// Whether this is the primary email address for the account
    pub is_primary: bool,

    /// ISO 8601 timestamp when the email was added
    pub added_at: String,
}

/// Public metadata for an account.
///
/// Information that is visible to other users and can be displayed in public profiles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProductAccountMetadataPublic {
    /// Optional biographical information
    pub bio: Option<String>,

    /// Verified domains associated with the account
    pub domains: Option<Vec<AccountDomain>>,

    /// Geographic location of the account holder
    pub location: Option<String>,

    /// Owner account ID (for organizational accounts)
    pub owner_account_id: Option<String>,

    /// List of admin account IDs (for organizational accounts)
    pub admin_account_ids: Option<Vec<String>>,

    /// List of member account IDs (for organizational accounts)
    pub member_account_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataConnectionDetails {
    pub provider: String,
    pub region: Option<String>,
    pub base_prefix: Option<String>,
    pub bucket: Option<String>,
    pub account_name: Option<String>,
    pub container_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataConnectionAuthentication {
    #[serde(rename = "type")]
    pub auth_type: String,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataConnection {
    pub data_connection_id: String,
    pub name: String,
    pub prefix_template: String,
    pub read_only: bool,
    pub allowed_data_modes: Vec<String>,
    pub required_flag: Option<String>,
    pub details: DataConnectionDetails,
    pub authentication: Option<DataConnectionAuthentication>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProductList {
    pub products: Vec<SourceProduct>,
    pub next: Option<String>,
}

#[async_trait]
impl Api for SourceApi {
    /// Creates and returns a backend client for a specific repository.
    ///
    /// This method determines the appropriate storage backend (S3 or Azure) based on
    /// the repository's configuration and returns a boxed `Repository` trait object.
    ///
    /// # Arguments
    ///
    /// * `account_id` - The ID of the account owning the repository.
    /// * `repository_id` - The ID of the repository.
    ///
    /// # Returns
    ///
    /// Returns a `Result` containing either a boxed `Repository` trait object
    /// or an empty error `()` if the client creation fails.
    async fn get_backend_client(
        &self,
        account_id: &str,
        repository_id: &str,
    ) -> Result<Box<dyn Repository>, BackendError> {
        let product = self
            .get_repository_record(account_id, repository_id)
            .await?;

        let Some(repository_data) = product
            .metadata
            .mirrors
            .get(product.metadata.primary_mirror.as_str())
        else {
            return Err(BackendError::SourceRepositoryMissingPrimaryMirror);
        };

        let data_connection_id = repository_data.connection_id.clone();
        let data_connection = self.get_data_connection(&data_connection_id).await?;

        match data_connection.details.provider.as_str() {
            "s3" => {
                let region =
                    if data_connection.authentication.clone().unwrap().auth_type == "s3_local" {
                        Region::Custom {
                            name: data_connection
                                .details
                                .region
                                .clone()
                                .unwrap_or("us-west-2".to_string()),
                            endpoint: "http://localhost:5050".to_string(),
                        }
                    } else {
                        Region::Custom {
                            name: data_connection
                                .details
                                .region
                                .clone()
                                .unwrap_or("us-east-1".to_string()),
                            endpoint: format!(
                                "https://s3.{}.amazonaws.com",
                                data_connection
                                    .details
                                    .region
                                    .clone()
                                    .unwrap_or("us-east-1".to_string())
                            ),
                        }
                    };

                let bucket: String = data_connection.details.bucket.clone().unwrap_or_default();
                let base_prefix: String = data_connection
                    .details
                    .base_prefix
                    .clone()
                    .unwrap_or_default();

                let mut prefix = format!("{}{}", base_prefix, repository_data.prefix);
                if prefix.ends_with('/') {
                    prefix = prefix[..prefix.len() - 1].to_string();
                };

                let auth = data_connection.authentication.clone().unwrap();

                Ok(Box::new(S3Repository {
                    account_id: account_id.to_string(),
                    repository_id: repository_id.to_string(),
                    region,
                    bucket,
                    base_prefix: prefix,
                    auth_method: auth.auth_type,
                    access_key_id: auth.access_key_id,
                    secret_access_key: auth.secret_access_key,
                }))
            }
            "az" => {
                let account_name: String = data_connection
                    .details
                    .account_name
                    .clone()
                    .unwrap_or_default();

                let container_name: String = data_connection
                    .details
                    .container_name
                    .clone()
                    .unwrap_or_default();

                let base_prefix: String = data_connection
                    .details
                    .base_prefix
                    .clone()
                    .unwrap_or_default();

                Ok(Box::new(AzureRepository {
                    account_id: account_id.to_string(),
                    repository_id: repository_id.to_string(),
                    account_name,
                    container_name,
                    base_prefix: format!("{}{}", base_prefix, repository_data.prefix),
                }))
            }
            err => Err(BackendError::UnexpectedDataConnectionProvider {
                provider: err.to_string(),
            }),
        }
    }

    async fn get_account(
        &self,
        account_id: String,
        user_identity: UserIdentity,
    ) -> Result<Account, BackendError> {
        let client = self.build_req_client();
        // Create headers
        let mut headers = self.build_source_headers();
        if user_identity.api_key.is_some() {
            let api_key = user_identity.api_key.unwrap();
            headers.insert(
                reqwest::header::AUTHORIZATION,
                reqwest::header::HeaderValue::from_str(
                    format!("{} {}", api_key.access_key_id, api_key.secret_access_key).as_str(),
                )
                .unwrap(),
            );
        }

        let response = client
            .get(format!("{}/api/v1/products/{}", self.endpoint, account_id))
            .headers(headers)
            .send()
            .await?;

        let product_list =
            process_json_response::<SourceProductList>(response, BackendError::RepositoryNotFound)
                .await?;
        let mut account = Account::default();

        for product in product_list.products {
            account.repositories.push(product.product_id);
        }

        Ok(account)
    }
}

impl SourceApi {
    /// Creates a new Source API client with the specified configuration.
    ///
    /// # Arguments
    ///
    /// * `endpoint` - Base URL for the Source API (e.g., "https://api.source.coop")
    /// * `api_key` - API key for authenticating requests
    /// * `proxy_url` - Optional proxy URL for requests (e.g., "http://proxy:8080")
    ///
    /// # Examples
    ///
    /// ```rust
    /// use source_data_proxy::apis::source::SourceApi;
    ///
    /// let api = SourceApi::new(
    ///     "https://api.source.coop".to_string(),
    ///     "your-api-key".to_string(),
    ///     None
    /// );
    /// ```
    pub fn new(endpoint: String, api_key: String, proxy_url: Option<String>) -> Self {
        let product_cache = Arc::new(
            Cache::builder()
                .time_to_live(Duration::from_secs(60)) // Set TTL to 60 seconds
                .build(),
        );

        let data_connection_cache = Arc::new(
            Cache::builder()
                .time_to_live(Duration::from_secs(60)) // Set TTL to 60 seconds
                .build(),
        );

        let access_key_cache = Arc::new(
            Cache::builder()
                .time_to_live(Duration::from_secs(60)) // Set TTL to 60 seconds
                .build(),
        );

        let permissions_cache = Arc::new(
            Cache::builder()
                .time_to_live(Duration::from_secs(60)) // Set TTL to 60 seconds
                .build(),
        );

        SourceApi {
            endpoint,
            api_key,
            product_cache,
            data_connection_cache,
            access_key_cache,
            permissions_cache,
            proxy_url,
        }
    }

    /// Creates a new `reqwest::Client` with the appropriate proxy settings.
    ///
    /// # Returns
    ///
    /// Returns a `reqwest::Client` with the appropriate proxy settings.
    fn build_req_client(&self) -> reqwest::Client {
        let mut client = reqwest::Client::builder();
        if let Some(proxy) = &self.proxy_url {
            client = client.proxy(reqwest::Proxy::all(proxy).unwrap());
        }
        client.build().unwrap()
    }

    /// Builds the headers for the Source API.
    ///
    /// # Returns
    ///
    /// Returns a `reqwest::header::HeaderMap` with the appropriate headers.
    fn build_source_headers(&self) -> reqwest::header::HeaderMap {
        const CORE_REQUEST_HEADERS: &[(&str, &str)] = &[
            ("accept", "application/json"),
            (
                "user-agent",
                concat!("source-proxy/", env!("CARGO_PKG_VERSION")),
            ),
        ];
        CORE_REQUEST_HEADERS
            .iter()
            .map(|(name, value)| {
                (
                    reqwest::header::HeaderName::from_lowercase(name.as_bytes()).unwrap(),
                    reqwest::header::HeaderValue::from_str(value).unwrap(),
                )
            })
            .collect()
    }

    /// Retrieves a product record by account and product ID.
    ///
    /// This method fetches product information from the Source API, including
    /// metadata, account details, and configuration. Results are cached for
    /// improved performance.
    ///
    /// # Arguments
    ///
    /// * `account_id` - The ID of the account that owns the product
    /// * `repository_id` - The ID of the product to retrieve
    ///
    /// # Returns
    ///
    /// Returns a `Result` containing either a `SourceProduct` struct with the
    /// product information or a `BackendError` if the request fails.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use source_data_proxy::apis::source::SourceApi;
    ///
    /// let api = SourceApi::new(
    ///     "https://api.source.coop".to_string(),
    ///     "your-api-key".to_string(),
    ///     None
    /// );
    ///
    /// let product = api.get_repository_record("example-account", "example-product").await?;
    /// println!("Product: {}", product.title);
    /// ```
    pub async fn get_repository_record(
        &self,
        account_id: &str,
        repository_id: &str,
    ) -> Result<SourceProduct, BackendError> {
        // Try to get the cached value
        let cache_key = format!("{account_id}/{repository_id}");

        if let Some(cached_repo) = self.product_cache.get(&cache_key).await {
            return Ok(cached_repo);
        }

        // If not in cache, fetch it
        let url = format!(
            "{}/api/v1/products/{}/{}",
            self.endpoint, account_id, repository_id
        );
        let client = self.build_req_client();
        let headers = self.build_source_headers();
        let response = client.get(url).headers(headers).send().await?;
        let repository =
            process_json_response::<SourceProduct>(response, BackendError::RepositoryNotFound)
                .await?;

        // Cache the successful result
        self.product_cache
            .insert(cache_key, repository.clone())
            .await;
        Ok(repository)
    }

    async fn fetch_data_connection(
        &self,
        data_connection_id: &str,
    ) -> Result<DataConnection, BackendError> {
        let client = self.build_req_client();
        let mut headers = self.build_source_headers();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&self.api_key).unwrap(),
        );

        let response = client
            .get(format!(
                "{}/api/v1/data-connections/{}",
                self.endpoint, data_connection_id
            ))
            .headers(headers)
            .send()
            .await?;
        process_json_response::<DataConnection>(response, BackendError::DataConnectionNotFound)
            .await
    }

    async fn get_data_connection(
        &self,
        data_connection_id: &str,
    ) -> Result<DataConnection, BackendError> {
        if let Some(cached_repo) = self.data_connection_cache.get(data_connection_id).await {
            return Ok(cached_repo);
        }

        // If not in cache, fetch it
        match self.fetch_data_connection(data_connection_id).await {
            Ok(data_connection) => {
                // Cache the successful result
                self.data_connection_cache
                    .insert(data_connection_id.to_string(), data_connection.clone())
                    .await;
                Ok(data_connection)
            }
            Err(e) => Err(e),
        }
    }

    pub async fn get_api_key(&self, access_key_id: &str) -> Result<APIKey, BackendError> {
        if let Some(cached_secret) = self.access_key_cache.get(access_key_id).await {
            return Ok(cached_secret);
        }

        // If not in cache, fetch it
        if access_key_id.is_empty() {
            let secret = APIKey {
                access_key_id: "".to_string(),
                secret_access_key: "".to_string(),
            };
            self.access_key_cache
                .insert(access_key_id.to_string(), secret.clone())
                .await;
            Ok(secret)
        } else {
            let secret = self.fetch_api_key(access_key_id.to_string()).await?;
            self.access_key_cache
                .insert(access_key_id.to_string(), secret.clone())
                .await;
            Ok(secret)
        }
    }

    async fn fetch_api_key(&self, access_key_id: String) -> Result<APIKey, BackendError> {
        let client = self.build_req_client();

        // Create headers
        let mut headers = self.build_source_headers();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&self.api_key).unwrap(),
        );
        let response = client
            .get(format!(
                "{}/api/v1/api-keys/{access_key_id}/auth",
                self.endpoint
            ))
            .headers(headers)
            .send()
            .await?;
        let key = process_json_response::<APIKey>(response, BackendError::ApiKeyNotFound).await?;

        Ok(APIKey {
            access_key_id,
            secret_access_key: key.secret_access_key,
        })
    }

    pub async fn is_authorized(
        &self,
        user_identity: UserIdentity,
        account_id: &str,
        repository_id: &str,
        permission: RepositoryPermission,
    ) -> Result<bool, BackendError> {
        let anon: bool = user_identity.api_key.is_none();

        // Try to get the cached value
        let cache_key = if anon {
            format!("{account_id}/{repository_id}")
        } else {
            let api_key = user_identity.clone().api_key.unwrap();
            format!("{}/{}/{}", account_id, repository_id, api_key.access_key_id)
        };

        if let Some(cache_permissions) = self.permissions_cache.get(&cache_key).await {
            return Ok(cache_permissions.contains(&permission));
        }

        // If not in cache, fetch it
        let permissions = self
            .fetch_permission(user_identity.clone(), account_id, repository_id)
            .await?;

        // Cache the successful result
        self.permissions_cache
            .insert(cache_key, permissions.clone())
            .await;

        Ok(permissions.contains(&permission))
    }

    pub async fn assert_authorized(
        &self,
        user_identity: UserIdentity,
        account_id: &str,
        repository_id: &str,
        permission: RepositoryPermission,
    ) -> Result<bool, BackendError> {
        let authorized = self
            .is_authorized(user_identity, account_id, repository_id, permission)
            .await?;
        if !authorized {
            return Err(BackendError::UnauthorizedError);
        }
        Ok(authorized)
    }

    async fn fetch_permission(
        &self,
        user_identity: UserIdentity,
        account_id: &str,
        repository_id: &str,
    ) -> Result<Vec<RepositoryPermission>, BackendError> {
        let client = self.build_req_client();

        // Create headers
        let mut headers = self.build_source_headers();
        if user_identity.api_key.is_some() {
            let api_key = user_identity.api_key.unwrap();
            headers.insert(
                reqwest::header::AUTHORIZATION,
                reqwest::header::HeaderValue::from_str(
                    format!("{} {}", api_key.access_key_id, api_key.secret_access_key).as_str(),
                )
                .unwrap(),
            );
        }

        let response = client
            .get(format!(
                "{}/api/v1/products/{account_id}/{repository_id}/permissions",
                self.endpoint
            ))
            .headers(headers)
            .send()
            .await?;

        process_json_response::<Vec<RepositoryPermission>>(
            response,
            BackendError::RepositoryPermissionsNotFound,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn test_json_parsing() {
        let json_str = r#"
        {
          "updated_at": "2023-01-15T10:30:00.000Z",
          "metadata": {
            "primary_mirror": "aws-us-east-1",
            "mirrors": {
              "aws-us-east-1": {
                "storage_type": "s3",
                "is_primary": true,
                "connection_id": "aws-connection-123",
                "config": { "region": "us-east-1", "bucket": "example-bucket" },
                "prefix": "example-account/sample-product/"
              }
            },
            "tags": ["example", "test"],
            "roles": {
              "example-account": {
                "granted_at": "2023-01-15T10:30:00.000Z",
                "account_id": "example-account",
                "role": "admin",
                "granted_by": "example-account"
              }
            }
          },
          "created_at": "2023-01-01T00:00:00.000Z",
          "disabled": false,
          "visibility": "public",
          "data_mode": "open",
          "account_id": "example-account",
          "description": "An example product for testing purposes.",
          "product_id": "sample-product",
          "featured": 0,
          "title": "Sample Product",
          "account": {
            "identity_id": "12345678-1234-1234-1234-123456789abc",
            "metadata_public": {
              "domains": [
                {
                  "created_at": "2023-01-10T12:00:00.000Z",
                  "domain": "example.com",
                  "status": "unverified"
                }
              ],
              "location": "Example City"
            },
            "updated_at": "2023-01-15T10:30:00.000Z",
            "flags": ["create_repositories", "create_organizations"],
            "created_at": "2023-01-01T00:00:00.000Z",
            "emails": [
              {
                "verified": false,
                "added_at": "2023-01-01T00:00:00.000Z",
                "address": "user@example.com",
                "is_primary": true
              }
            ],
            "disabled": false,
            "metadata_private": {},
            "account_id": "example-account",
            "name": "Example User",
            "type": "individual"
          }
        }
        "#;

        match serde_json::from_str::<SourceProduct>(json_str) {
            Ok(_product) => {
                println!("✅ JSON parsed successfully!");
            }
            Err(e) => {
                panic!("❌ JSON parsing failed: {}", e);
            }
        }
    }
}
