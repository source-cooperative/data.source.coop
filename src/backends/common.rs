use crate::utils::errors::APIError;
use async_trait::async_trait;
use bytes::Bytes;
use core::num::NonZeroU32;
use futures_core::Stream;
use serde::Serialize;
use std::pin::Pin;

use reqwest::Error as ReqwestError;
type BoxedReqwestStream = Pin<Box<dyn Stream<Item = Result<Bytes, ReqwestError>> + Send>>;

pub fn parse_s3_uri(uri: &str) -> Result<(String, String), &'static str> {
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

pub struct GetObjectResponse {
    pub content_length: u64,
    pub content_type: String,
    pub last_modified: String,
    pub etag: String,
    pub body: BoxedReqwestStream,
}

pub struct HeadObjectResponse {
    pub content_length: u64,
    pub content_type: String,
    pub last_modified: String,
    pub etag: String,
}

#[async_trait]
pub trait Repository {
    async fn get_object(
        &self,
        key: String,
        range: Option<String>,
    ) -> Result<GetObjectResponse, Box<dyn APIError>>;
    async fn head_object(&self, key: String) -> Result<HeadObjectResponse, Box<dyn APIError>>;
    async fn list_objects_v2(
        &self,
        prefix: String,
        continuation_token: Option<String>,
        max_keys: NonZeroU32,
    ) -> Result<ListBucketResult, Box<dyn APIError>>;
}

#[derive(Debug, Serialize)]
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
    pub common_prefixes: Vec<CommonPrefix>,
    #[serde(rename = "NextContinuationToken")]
    pub next_continuation_token: Option<String>,
}

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
pub struct CommonPrefix {
    #[serde(rename = "Prefix")]
    pub prefix: String,
}
