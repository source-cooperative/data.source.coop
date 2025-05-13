use super::{Account, API};
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
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;
#[derive(Clone)]
pub struct SourceAPI {
    pub endpoint: String,
    repository_cache: Arc<Cache<String, SourceRepository>>,
    data_connection_cache: Arc<Cache<String, DataConnection>>,
    api_key_cache: Arc<Cache<String, APIKey>>,
    permissions_cache: Arc<Cache<String, Vec<RepositoryPermission>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RepositoryPermission {
    #[serde(rename = "read")]
    Read,
    #[serde(rename = "write")]
    Write,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct APIKey {
    pub access_key_id: String,
    pub secret_access_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRepository {
    pub account_id: String,
    pub repository_id: String,
    pub data_mode: String,
    pub disabled: bool,
    pub featured: u8,
    pub published: String,
    pub state: String,
    pub meta: SourceRepositoryMeta,
    pub data: SourceRepositoryData,
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
pub struct SourceRepositoryMeta {
    pub description: String,
    pub title: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRepositoryData {
    pub primary_mirror: String,
    pub mirrors: HashMap<String, SourceRepositoryMirror>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRepositoryMirror {
    pub prefix: String,
    pub data_connection_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRepositoryList {
    pub repositories: Vec<SourceRepository>,
    pub next: Option<String>,
}

#[async_trait]
impl API for SourceAPI {
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
        account_id: &String,
        repository_id: &String,
    ) -> Result<Box<dyn Repository>, BackendError> {
        let repository = self
            .get_repository_record(&account_id, &repository_id)
            .await?;

        let repository_data = match repository
            .data
            .mirrors
            .get(repository.data.primary_mirror.as_str())
        {
            Some(repository_data) => repository_data,
            None => {
                return Err(BackendError::SourceRepositoryMissingPrimaryMirror);
            }
        };

        let data_connection_id = repository_data.data_connection_id.clone();
        let data_connection = self.get_data_connection(&data_connection_id).await?;

        match data_connection.details.provider.as_str() {
            "s3" => {
                let region: Region;

                if data_connection.authentication.clone().unwrap().auth_type == "s3_local" {
                    region = Region::Custom {
                        name: data_connection
                            .details
                            .region
                            .clone()
                            .unwrap_or("us-west-2".to_string()),
                        endpoint: format!("http://localhost:5050"),
                    };
                } else {
                    region = Region::Custom {
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
                    };
                }

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
        let client = reqwest::Client::new();
        // Create headers
        let mut headers = reqwest::header::HeaderMap::new();
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
                "{}/api/v1/repositories/{}",
                self.endpoint, account_id
            ))
            .headers(headers)
            .send()
            .await?;

        let repository_list = process_json_response::<SourceRepositoryList>(
            response,
            BackendError::RepositoryNotFound,
        )
        .await?;
        let mut account = Account::default();

        for repository in repository_list.repositories {
            account.repositories.push(repository.repository_id);
        }

        Ok(account)
    }
}

impl SourceAPI {
    pub fn new(endpoint: String) -> Self {
        let repository_cache = Arc::new(
            Cache::builder()
                .time_to_live(Duration::from_secs(60)) // Set TTL to 60 seconds
                .build(),
        );

        let data_connection_cache = Arc::new(
            Cache::builder()
                .time_to_live(Duration::from_secs(60)) // Set TTL to 60 seconds
                .build(),
        );

        let api_key_cache = Arc::new(
            Cache::builder()
                .time_to_live(Duration::from_secs(60)) // Set TTL to 60 seconds
                .build(),
        );

        let permissions_cache = Arc::new(
            Cache::builder()
                .time_to_live(Duration::from_secs(60)) // Set TTL to 60 seconds
                .build(),
        );

        SourceAPI {
            endpoint,
            repository_cache,
            data_connection_cache,
            api_key_cache,
            permissions_cache,
        }
    }

    /// Retrieves the repository record for a given account and repository ID.
    ///
    /// # Arguments
    ///
    /// * `account_id` - The ID of the account owning the repository.
    /// * `repository_id` - The ID of the repository.
    ///
    /// # Returns
    ///
    /// Returns a `Result` containing either a `SourceRepository` struct with the
    /// repository information or a BackendError if the request fails.
    pub async fn get_repository_record(
        &self,
        account_id: &String,
        repository_id: &String,
    ) -> Result<SourceRepository, BackendError> {
        // Try to get the cached value
        let cache_key = format!("{}/{}", account_id, repository_id);

        if let Some(cached_repo) = self.repository_cache.get(&cache_key).await {
            return Ok(cached_repo);
        }

        // If not in cache, fetch it
        match self.fetch_repository(account_id, repository_id).await {
            Ok(repository) => {
                // Cache the successful result
                self.repository_cache
                    .insert(cache_key, repository.clone())
                    .await;
                Ok(repository)
            }
            Err(e) => Err(e),
        }
    }

    async fn fetch_data_connection(
        &self,
        data_connection_id: &String,
    ) -> Result<DataConnection, BackendError> {
        let source_key = env::var("SOURCE_KEY").unwrap();
        let client = reqwest::Client::new();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&source_key).unwrap(),
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
        data_connection_id: &String,
    ) -> Result<DataConnection, BackendError> {
        // Try to get the cached value
        let cache_key = format!("{}", data_connection_id);

        if let Some(cached_repo) = self.data_connection_cache.get(&cache_key).await {
            return Ok(cached_repo);
        }

        // If not in cache, fetch it
        match self.fetch_data_connection(data_connection_id).await {
            Ok(data_connection) => {
                // Cache the successful result
                self.data_connection_cache
                    .insert(cache_key, data_connection.clone())
                    .await;
                Ok(data_connection)
            }
            Err(e) => Err(e),
        }
    }

    pub async fn get_api_key(&self, access_key_id: String) -> Result<APIKey, BackendError> {
        // Try to get the cached value
        let cache_key = format!("{}", access_key_id);

        if let Some(cached_secret) = self.api_key_cache.get(&cache_key).await {
            return Ok(cached_secret);
        }

        // If not in cache, fetch it
        let secret = self.fetch_api_key(access_key_id).await?;
        
        // Cache the successful result
        if let Some(secret) = secret {
            self.api_key_cache.insert(cache_key, secret.clone()).await;
            Ok(secret)
        } else {
            let secret = APIKey {
                access_key_id: "".to_string(),
                secret_access_key: "".to_string(),
            };
            self.api_key_cache.insert(cache_key, secret.clone()).await;
            Ok(secret)
        }
    }

    async fn fetch_api_key(&self, access_key_id: String) -> Result<Option<APIKey>, BackendError> {
        if access_key_id.is_empty() {
            return Ok(None);
        }
        let client = reqwest::Client::new();
        let source_key = env::var("SOURCE_KEY").unwrap();
        let source_api_url = env::var("SOURCE_API_URL").unwrap();

        // Create headers
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&source_key).unwrap(),
        );
        let response = client
            .get(format!(
                "{}/api/v1/api-keys/{}/auth",
                source_api_url, access_key_id
            ))
            .headers(headers)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(BackendError::UnexpectedApiError(format!(
                "Failed to get API key for access key id: {}, {}",
                access_key_id, text
            )));
        }
        let json: Value = serde_json::from_str(&text).unwrap();
        let secret_access_key = json["secret_access_key"].as_str().unwrap();

        Ok(Some(APIKey {
            access_key_id,
            secret_access_key: secret_access_key.to_string(),
        }))
    }

    async fn fetch_repository(
        &self,
        account_id: &String,
        repository_id: &String,
    ) -> Result<SourceRepository, BackendError> {
        let response = reqwest::get(format!(
            "{}/api/v1/repositories/{}/{}",
            self.endpoint, account_id, repository_id
        ))
        .await?;
        process_json_response::<SourceRepository>(response, BackendError::RepositoryNotFound).await
    }

    pub async fn is_authorized(
        &self,
        user_identity: UserIdentity,
        account_id: &String,
        repository_id: &String,
        permission: RepositoryPermission,
    ) -> Result<bool, BackendError> {
        let anon: bool;
        if user_identity.api_key.is_none() {
            anon = true;
        } else {
            anon = false;
        }

        // Try to get the cached value
        let cache_key: String;
        if anon {
            cache_key = format!("{}/{}", account_id, repository_id);
        } else {
            let api_key = user_identity.clone().api_key.unwrap();
            cache_key = format!("{}/{}/{}", account_id, repository_id, api_key.access_key_id);
        }

        if let Some(cache_permissions) = self.permissions_cache.get(&cache_key).await {
            return Ok(cache_permissions.contains(&permission));
        }

        // If not in cache, fetch it
        let permissions = self
            .fetch_permission(user_identity.clone(), &account_id, &repository_id)
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
        account_id: &String,
        repository_id: &String,
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
        account_id: &String,
        repository_id: &String,
    ) -> Result<Vec<RepositoryPermission>, BackendError> {
        let client = reqwest::Client::new();
        let source_api_url = env::var("SOURCE_API_URL").unwrap();

        // Create headers
        let mut headers = reqwest::header::HeaderMap::new();
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
                "{}/api/v1/repositories/{}/{}/permissions",
                source_api_url, account_id, repository_id
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
