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

mod types;

// Re-export all types
pub use types::*;

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
