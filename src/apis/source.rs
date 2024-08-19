use super::API;
use crate::backends::common::{parse_s3_uri, Repository};
use crate::utils::errors::{APIError, InternalServerError, RepositoryNotFoundError};
use async_trait::async_trait;
use rusoto_core::Region;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::backends::azure::AzureRepository;
use crate::backends::s3::S3Repository;
use crate::utils::core::parse_azure_blob_url;

/// Represents an API client for interacting with source repositories.
///
/// This struct provides methods to retrieve repository information and create
/// backend clients for different storage providers (S3 or Azure).
///
/// # Fields
///
/// * `endpoint` - The base URL of the source API.
pub struct SourceAPI {
    pub endpoint: String,
}

#[derive(Serialize, Deserialize)]
pub struct SourceRepository {
    pub account_id: String,
    pub repository_id: String,
    pub data_mode: String,
    pub disabled: bool,
    pub featured: u8,
    pub mode: String,
    pub meta: SourceRepositoryMeta,
    pub data: SourceRepositoryData,
}

#[derive(Serialize, Deserialize)]
pub struct SourceRepositoryMeta {
    pub description: String,
    pub published: String,
    pub title: String,
    pub tags: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct SourceRepositoryData {
    pub cdn: String,
    pub primary_mirror: String,
    pub mirrors: HashMap<String, SourceRepositoryMirror>,
}

#[derive(Serialize, Deserialize)]
pub struct SourceRepositoryMirror {
    pub name: String,
    pub provider: String,
    pub region: Option<String>,
    pub uri: Option<String>,
    pub delimiter: Option<String>,
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
        account_id: String,
        repository_id: String,
    ) -> Result<Box<dyn Repository>, ()> {
        match self
            .get_repository_record(&account_id, &repository_id)
            .await
        {
            Ok(repository) => {
                match repository
                    .data
                    .mirrors
                    .get(repository.data.primary_mirror.as_str())
                {
                    Some(repository_data) => {
                        if &repository_data.provider == "s3" {
                            let region = Region::Custom {
                                name: repository_data
                                    .region
                                    .clone()
                                    .unwrap_or("us-east-1".to_string()),
                                endpoint: format!(
                                    "https://s3.{}.amazonaws.com",
                                    repository_data
                                        .region
                                        .clone()
                                        .unwrap_or("us-east-1".to_string())
                                ),
                            };

                            let uri = repository_data.uri.clone().unwrap_or_default();

                            match parse_s3_uri(uri.as_str()) {
                                Ok((bucket, base_prefix)) => Ok(Box::new(S3Repository {
                                    account_id: account_id.to_string(),
                                    repository_id: repository_id.to_string(),
                                    region,
                                    bucket,
                                    base_prefix,
                                    delimiter: repository_data
                                        .delimiter
                                        .clone()
                                        .unwrap_or("/".to_string()),
                                })),
                                Err(_) => Err(()),
                            }
                        } else {
                            // This is an Azure backed repository
                            let uri = repository_data.uri.clone().unwrap_or_default();
                            let result = parse_azure_blob_url(uri);

                            if result.is_err() {
                                return Err(());
                            }

                            let (account_name, container_name, base_prefix) = result.unwrap();

                            Ok(Box::new(AzureRepository {
                                account_id: account_id.to_string(),
                                repository_id: repository_id.to_string(),
                                account_name,
                                container_name,
                                base_prefix,
                                delimiter: "/".to_string(),
                            }))
                        }
                    }
                    None => {
                        return Err(());
                    }
                }
            }
            Err(_) => Err(()),
        }
    }
}

impl SourceAPI {
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
    /// repository information or a boxed `APIError` if the request fails.
    pub async fn get_repository_record(
        &self,
        account_id: &String,
        repository_id: &String,
    ) -> Result<SourceRepository, Box<dyn APIError>> {
        match reqwest::get(format!(
            "{}/repositories/{}/{}",
            self.endpoint, account_id, repository_id
        ))
        .await
        {
            Ok(response) => match response.json::<SourceRepository>().await {
                Ok(repository) => Ok(repository),
                Err(_) => Err(Box::new(InternalServerError {
                    message: "Internal Server Error".to_string(),
                })),
            },
            Err(error) => {
                dbg!(&error);
                if error.status().is_some() && error.status().unwrap().as_u16() == 404 {
                    return Err(Box::new(RepositoryNotFoundError {
                        account_id: account_id.to_string(),
                        repository_id: repository_id.to_string(),
                    }));
                }

                Err(Box::new(InternalServerError {
                    message: "Internal Server Error".to_string(),
                }))
            }
        }
    }
}
