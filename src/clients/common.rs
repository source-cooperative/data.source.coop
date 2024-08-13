use std::str::FromStr;

use serde::Serialize;
use async_trait::async_trait;
use crate::clients::s3::S3Repository;
use crate::clients::azure::AzureRepository;
use rusoto_core::Region;
use crate::utils::repository::get_repository_record;


fn parse_s3_uri(uri: &str) -> Result<(String, String), &'static str> {
    // Check if the URI starts with "s3://"
    if !uri.starts_with("s3://") {
        return Err("Invalid S3 URI: must start with 's3://'");
    }

    // Remove the "s3://" prefix
    let uri = &uri[5..];

    // Find the first '/' after the bucket name
    match uri.find('/') {
        Some(slash_index) => {
            let (bucket, prefix) = uri.split_at(slash_index);
            // Remove the leading '/' from the prefix
            Ok((bucket.to_string(), prefix[1..].to_string()))
        }
        None => {
            // If there's no '/', the entire string is the bucket name
            Ok((uri.to_string(), String::new()))
        }
    }
}

#[async_trait]
pub trait Repository {
    async fn list_objects_v2(&self, prefix: String) -> Result<ListBucketResult, ()>;
}


#[derive(Serialize)]
pub struct ListBucketResult {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Prefix")]
    pub prefix: String,
    #[serde(rename = "KeyCount")]
    pub key_count: i64,
    #[serde(rename = "MaxKeys")]
    pub max_keys: i64,
    #[serde(rename = "IsTruncated")]
    pub is_truncated: bool,
    #[serde(rename = "Contents")]
    pub contents: Vec<Content>,
    #[serde(rename = "CommonPrefixes")]
    pub common_prefixes: Vec<CommonPrefix>
}

#[derive(Serialize)]
pub struct Content {
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "LastModified")]
    pub last_modified: String,
    #[serde(rename = "ETag")]
    pub etag: String,
    #[serde(rename = "Size")]
    pub size: i64,
    #[serde(rename = "StorageClass")]
    pub storage_class: String,
}

#[derive(Serialize)]
pub struct CommonPrefix {
    #[serde(rename = "Prefix")]
    pub prefix: String
}

// TODO: Find a way to clean this up
pub async fn fetch_repository_client(account_id: &String, repository_id: &String) -> Result<Box<dyn Repository>, String> {
    match get_repository_record(&account_id, &repository_id).await {
        Ok(repository) => {
            match repository.data.mirrors.get(repository.data.primary_mirror.as_str()) {
                Some(repository_data) => {
                    if &repository_data.provider == "s3" {
                        let region = Region::from_str(repository_data.region.clone().unwrap_or("us-east-1".to_string()).as_str()).unwrap_or(Region::UsEast1);
                        let uri = repository_data.uri.clone().unwrap_or_default();

                        match parse_s3_uri(uri.as_str()) {
                            Ok((bucket, base_prefix)) => {
                                Ok(
                                    Box::new(S3Repository {
                                        account_id: account_id.to_string(),
                                        repository_id: repository_id.to_string(),
                                        region,
                                        bucket,
                                        base_prefix,
                                        delimiter: repository_data.delimiter.clone().unwrap_or("/".to_string())
                                    })
                                )
                            }
                            Err(_) => {
                                Err("Some Error".to_string())
                            }
                        }
                    } else {
                        // This is an Azure backed repository
                        Ok(
                            Box::new(AzureRepository {
                                account_id: account_id.to_string(),
                                repository_id: repository_id.to_string(),
                                account_name: "radiantearth".to_string(),
                                container_name: "mlhub".to_string(),
                                base_prefix: "landcovernet".to_string(),
                                delimiter: "/".to_string()
                            })
                        )
                    }
                }
                None => {
                    return Err("Primary mirror not found".to_string());
                }
            }
        }
        Err(_) => {
            Err("Failed to fetch repository record".to_string())
        }
    }
}
