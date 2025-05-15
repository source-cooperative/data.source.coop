use async_trait::async_trait;
use bytes::Bytes;
use core::num::NonZeroU32;
use futures_core::Stream;
use serde::Deserialize;
use serde::Serialize;
use std::pin::Pin;

use reqwest::Error as ReqwestError;
type BoxedReqwestStream = Pin<Box<dyn Stream<Item = Result<Bytes, ReqwestError>> + Send>>;
use crate::utils::errors::BackendError;

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

#[derive(Debug, Serialize)]
pub struct CompleteMultipartUploadResponse {
    #[serde(rename = "Location")]
    pub location: String,
    #[serde(rename = "Bucket")]
    pub bucket: String,
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "ETag")]
    pub etag: String,
}

#[async_trait]
pub trait Repository {
    async fn delete_object(&self, key: String) -> Result<(), BackendError>;
    async fn create_multipart_upload(
        &self,
        key: String,
        content_type: Option<String>,
    ) -> Result<CreateMultipartUploadResponse, BackendError>;
    async fn abort_multipart_upload(
        &self,
        key: String,
        upload_id: String,
    ) -> Result<(), BackendError>;
    async fn complete_multipart_upload(
        &self,
        key: String,
        upload_id: String,
        parts: Vec<MultipartPart>,
    ) -> Result<CompleteMultipartUploadResponse, BackendError>;
    async fn upload_multipart_part(
        &self,
        key: String,
        upload_id: String,
        part_number: String,
        bytes: Bytes,
    ) -> Result<UploadPartResponse, BackendError>;
    async fn put_object(
        &self,
        key: String,
        bytes: Bytes,
        content_type: Option<String>,
    ) -> Result<(), BackendError>;
    async fn get_object(
        &self,
        key: String,
        range: Option<String>,
    ) -> Result<GetObjectResponse, BackendError>;
    async fn head_object(&self, key: String) -> Result<HeadObjectResponse, BackendError>;
    async fn list_objects_v2(
        &self,
        prefix: String,
        continuation_token: Option<String>,
        delimiter: Option<String>,
        max_keys: NonZeroU32,
    ) -> Result<ListBucketResult, BackendError>;
    async fn copy_object(
        &self,
        copy_identifier_path: String,
        key: String,
        range: Option<String>,
    ) -> Result<(), BackendError>;
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

#[derive(Debug, Serialize)]
pub struct CreateMultipartUploadResponse {
    #[serde(rename = "Bucket")]
    pub bucket: String,
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "UploadId")]
    pub upload_id: String,
}

#[derive(Debug, Serialize)]
pub struct UploadPartResponse {
    #[serde(rename = "ETag")]
    pub etag: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MultipartPart {
    #[serde(rename = "PartNumber")]
    pub part_number: i64,
    #[serde(rename = "ETag")]
    pub etag: String,
    #[serde(rename = "ChecksumCRC32")]
    pub checksum_crc32: Option<String>,
    #[serde(rename = "ChecksumCRC32C")]
    pub checksum_crc32c: Option<String>,
    #[serde(rename = "ChecksumSHA1")]
    pub checksum_sha1: Option<String>,
    #[serde(rename = "ChecksumSHA256")]
    pub checksum_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "CompleteMultipartUpload")]
pub struct CompleteMultipartUpload {
    #[serde(rename = "Part")]
    pub parts: Vec<MultipartPart>,
}
