use super::common::{MultipartPart, UploadPartResponse};
use crate::backends::common::{
    CommonPrefix, CompleteMultipartUploadResponse, Content, CreateMultipartUploadResponse,
    GetObjectResponse, HeadObjectResponse, ListBucketResult, Repository,
};
use crate::utils::core::replace_first;
use crate::utils::errors::BackendError;
use actix_web::http::header::RANGE;
use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use core::num::NonZeroU32;
use futures_core::Stream;
use reqwest;
use rusoto_core::Region;
use rusoto_s3::{
    AbortMultipartUploadRequest, CompleteMultipartUploadRequest, CompletedMultipartUpload,
    CompletedPart, CreateMultipartUploadRequest, DeleteObjectRequest, HeadObjectRequest,
    ListObjectsV2Request, PutObjectRequest, S3Client, UploadPartRequest, S3,
};
use std::pin::Pin;

pub struct S3Repository {
    pub account_id: String,
    pub repository_id: String,
    pub region: Region,
    pub bucket: String,
    pub base_prefix: String,
    pub auth_method: String,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
}

impl S3Repository {
    fn create_client(&self) -> Result<S3Client, BackendError> {
        if self.auth_method == "s3_access_key" {
            let credentials = rusoto_credential::StaticProvider::new_minimal(
                self.access_key_id.clone().unwrap(),
                self.secret_access_key.clone().unwrap(),
            );
            return Ok(S3Client::new_with(
                rusoto_core::request::HttpClient::new().unwrap(),
                credentials,
                self.region.clone(),
            ));
        } else if self.auth_method == "s3_ecs_task_role" {
            let credentials = rusoto_credential::ContainerProvider::new();
            return Ok(S3Client::new_with(
                rusoto_core::request::HttpClient::new().unwrap(),
                credentials,
                self.region.clone(),
            ));
        } else if self.auth_method == "s3_local" {
            let credentials = rusoto_credential::ChainProvider::new();
            return Ok(S3Client::new_with(
                rusoto_core::request::HttpClient::new().unwrap(),
                credentials,
                self.region.clone(),
            ));
        } else {
            return Err(BackendError::UnsupportedAuthMethod(format!(
                "Unsupported auth method: {}",
                self.auth_method
            )));
        }
    }
}

#[async_trait]
impl Repository for S3Repository {
    async fn get_object(
        &self,
        key: String,
        range: Option<String>,
    ) -> Result<GetObjectResponse, BackendError> {
        let head_object_response = self.head_object(key.clone()).await?;
        let client = reqwest::Client::new();
        let url: String;

        if self.auth_method == "s3_local" {
            url = format!(
                "http://localhost:5050/{}/{}/{}",
                self.bucket, self.base_prefix, key
            )
        } else {
            url = format!(
                "https://s3.{}.amazonaws.com/{}/{}/{}",
                self.region.name(),
                self.bucket,
                self.base_prefix,
                key
            );
        }
        // Start building the request
        let mut request = client.get(url);

        // If a range is provided, add it to the headers
        if let Some(range_value) = range {
            request = request.header(RANGE, range_value);
        }

        // Send the request and await the response
        let response = request.send().await?;
        // Get the byte stream from the response
        let content_length = response.content_length();
        let stream = response.bytes_stream();
        let boxed_stream: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>> =
            Box::pin(stream);

        Ok(GetObjectResponse {
            content_length: content_length.unwrap_or(0) as u64,
            content_type: head_object_response.content_type,
            etag: head_object_response.etag,
            last_modified: head_object_response.last_modified,
            body: boxed_stream,
        })
    }

    async fn put_object(
        &self,
        key: String,
        bytes: Bytes,
        content_type: Option<String>,
    ) -> Result<(), BackendError> {
        let client = self.create_client()?;

        let request = PutObjectRequest {
            bucket: self.bucket.clone(),
            key: format!("{}/{}", self.base_prefix, key),
            body: Some(bytes.to_vec().into()),
            content_type,
            ..Default::default()
        };

        client.put_object(request).await?;
        Ok(())
    }

    async fn create_multipart_upload(
        &self,
        key: String,
        content_type: Option<String>,
    ) -> Result<CreateMultipartUploadResponse, BackendError> {
        let client = self.create_client()?;

        let request = CreateMultipartUploadRequest {
            bucket: self.bucket.clone(),
            key: format!("{}/{}", self.base_prefix, key),
            content_type,
            ..Default::default()
        };

        let result = client.create_multipart_upload(request).await?;
        Ok(CreateMultipartUploadResponse {
            bucket: self.account_id.clone(),
            key: key.clone(),
            upload_id: result.upload_id.unwrap(),
        })
    }

    async fn abort_multipart_upload(
        &self,
        key: String,
        upload_id: String,
    ) -> Result<(), BackendError> {
        let client = self.create_client()?;

        let request = AbortMultipartUploadRequest {
            bucket: self.bucket.clone(),
            key: format!("{}/{}", self.base_prefix, key),
            upload_id,
            ..Default::default()
        };

        client.abort_multipart_upload(request).await?;
        Ok(())
    }

    async fn complete_multipart_upload(
        &self,
        key: String,
        upload_id: String,
        parts: Vec<MultipartPart>,
    ) -> Result<CompleteMultipartUploadResponse, BackendError> {
        let client = self.create_client()?;

        let request = CompleteMultipartUploadRequest {
            bucket: self.bucket.clone(),
            key: format!("{}/{}", self.base_prefix, key),
            upload_id,
            multipart_upload: Some(CompletedMultipartUpload {
                parts: Some(
                    parts
                        .iter()
                        .map(|part| CompletedPart {
                            e_tag: Some(part.etag.clone()),
                            part_number: Some(part.part_number),
                        })
                        .collect(),
                ),
            }),
            ..Default::default()
        };

        let result = client.complete_multipart_upload(request).await?;
        Ok(CompleteMultipartUploadResponse {
            location: "".to_string(),
            bucket: self.account_id.clone(),
            key: key.clone(),
            etag: result.e_tag.unwrap(),
        })
    }

    async fn upload_multipart_part(
        &self,
        key: String,
        upload_id: String,
        part_number: String,
        bytes: Bytes,
    ) -> Result<UploadPartResponse, BackendError> {
        let client = self.create_client()?;

        let request = UploadPartRequest {
            bucket: self.bucket.clone(),
            key: format!("{}/{}", self.base_prefix, key),
            upload_id,
            part_number: part_number.parse().unwrap(),
            body: Some(bytes.to_vec().into()),
            ..Default::default()
        };

        let result = client.upload_part(request).await?;
        Ok(UploadPartResponse {
            etag: result.e_tag.unwrap(),
        })
    }

    async fn delete_object(&self, key: String) -> Result<(), BackendError> {
        let client = self.create_client()?;

        let request = DeleteObjectRequest {
            bucket: self.bucket.clone(),
            key: format!("{}/{}", self.base_prefix, key),
            ..Default::default()
        };

        client.delete_object(request).await?;
        Ok(())
    }

    async fn head_object(&self, key: String) -> Result<HeadObjectResponse, BackendError> {
        let client = self.create_client()?;

        let request = HeadObjectRequest {
            bucket: self.bucket.clone(),
            key: format!("{}/{}", self.base_prefix, key),
            ..Default::default()
        };

        let result = client.head_object(request).await?;

        Ok(HeadObjectResponse {
            content_length: result.content_length.unwrap_or(0) as u64,
            content_type: result.content_type.unwrap_or_else(|| "".to_string()),
            etag: result.e_tag.unwrap_or_else(|| "".to_string()),
            last_modified: result
                .last_modified
                .unwrap_or_else(|| Utc::now().to_rfc2822()),
        })
    }

    async fn list_objects_v2(
        &self,
        prefix: String,
        continuation_token: Option<String>,
        delimiter: Option<String>,
        max_keys: NonZeroU32,
    ) -> Result<ListBucketResult, BackendError> {
        let client = self.create_client()?;

        let mut request = ListObjectsV2Request {
            bucket: self.bucket.clone(),
            prefix: Some(format!("{}/{}", self.base_prefix, prefix)),
            delimiter,
            max_keys: Some(max_keys.get() as i64),
            ..Default::default()
        };

        if let Some(token) = continuation_token {
            request.continuation_token = Some(token);
        }

        let output = client.list_objects_v2(request).await?;
        let result = ListBucketResult {
            name: format!("{}", self.account_id),
            prefix: format!("{}/{}", self.repository_id, prefix),
            key_count: output.key_count.unwrap_or(0),
            max_keys: output.max_keys.unwrap_or(0),
            is_truncated: output.is_truncated.unwrap_or(false),
            next_continuation_token: output.next_continuation_token,
            contents: output
                .contents
                .unwrap_or_default()
                .iter()
                .map(|item| Content {
                    key: replace_first(
                        item.key.clone().unwrap_or_else(|| "".to_string()),
                        self.base_prefix.clone(),
                        format!("{}", self.repository_id),
                    ),
                    last_modified: item
                        .last_modified
                        .clone()
                        .unwrap_or_else(|| Utc::now().to_rfc2822()),
                    etag: item.e_tag.clone().unwrap_or_else(|| "".to_string()),
                    size: item.size.unwrap_or(0),
                    storage_class: item.storage_class.clone().unwrap_or_else(|| "".to_string()),
                })
                .collect(),
            common_prefixes: output
                .common_prefixes
                .unwrap_or_default()
                .iter()
                .map(|item| CommonPrefix {
                    prefix: replace_first(
                        item.prefix.clone().unwrap_or_else(|| "".to_string()),
                        self.base_prefix.clone(),
                        format!("{}", self.repository_id),
                    ),
                })
                .collect(),
        };

        Ok(result)
    }

    async fn copy_object(
        &self,
        copy_identifier_path: String,
        key: String,
        range: Option<String>,
    ) -> Result<(), BackendError> {
        let client = self.create_client()?;

        let request = HeadObjectRequest {
            bucket: self.bucket.clone(),
            key: format!("{}", copy_identifier_path),
            ..Default::default()
        };

        let result = client.head_object(request).await?;
        let url_client = reqwest::Client::new();
        let url: String;

        if self.auth_method == "s3_local" {
            url = format!(
                "http://localhost:5050/{}/{}",
                self.bucket, copy_identifier_path
            )
        } else {
            url = format!(
                "https://s3.{}.amazonaws.com/{}/{}",
                self.region.name(),
                self.bucket,
                copy_identifier_path
            );
        }

        let mut request = url_client.get(url);

        if let Some(range_value) = range {
            request = request.header(RANGE, range_value);
        }

        let response = request.send().await?;
        let content_bytes = response
            .bytes()
            .await
            .unwrap_or_else(|_| bytes::Bytes::from(vec![]));
        self.put_object(key.clone(), content_bytes, result.content_type)
            .await?;
        Ok(())
    }
}
