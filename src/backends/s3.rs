use super::common::{MultipartPart, UploadPartResponse};
use crate::backends::common::{
    CommonPrefix, CompleteMultipartUploadResponse, Content, CreateMultipartUploadResponse,
    GetObjectResponse, HeadObjectResponse, ListBucketResult, Repository,
};
use crate::utils::core::replace_first;
use crate::utils::core::GenericByteStream;
use crate::utils::errors::{APIError, InternalServerError, ObjectNotFoundError};
use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use core::num::NonZeroU32;
use log::error;
use rusoto_core::Region;
use rusoto_core::RusotoError;
use rusoto_s3::{
    AbortMultipartUploadRequest, CompleteMultipartUploadRequest, CompletedMultipartUpload,
    CompletedPart, CreateMultipartUploadRequest, DeleteObjectRequest, GetObjectRequest,
    HeadObjectRequest, ListObjectsV2Request, PutObjectRequest, S3Client, UploadPartRequest, S3,
};
use std::fmt;

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

trait GetClient {
    fn get_client(&self) -> Result<S3Client, Box<dyn APIError>>;
}

impl GetClient for S3Repository {
    fn get_client(&self) -> Result<S3Client, Box<dyn APIError>> {
        let client: S3Client;

        if self.auth_method == "s3_access_key" {
            let credentials = rusoto_credential::StaticProvider::new_minimal(
                self.access_key_id.clone().unwrap(),
                self.secret_access_key.clone().unwrap(),
            );
            client = S3Client::new_with(
                rusoto_core::request::HttpClient::new().unwrap(),
                credentials,
                self.region.clone(),
            );
        } else if self.auth_method == "s3_ecs_task_role" {
            let credentials = rusoto_credential::ContainerProvider::new();
            client = S3Client::new_with(
                rusoto_core::request::HttpClient::new().unwrap(),
                credentials,
                self.region.clone(),
            );
        } else if self.auth_method == "s3_local" {
            let credentials = rusoto_credential::ChainProvider::new();
            client = S3Client::new_with(
                rusoto_core::request::HttpClient::new().unwrap(),
                credentials,
                self.region.clone(),
            );
        } else {
            return Err(Box::new(InternalServerError {
                message: format!("Internal Server Error"),
            }));
        }

        return Ok(client);
    }
}

#[async_trait]
impl Repository for S3Repository {
    async fn get_object(
        &self,
        key: String,
        range: Option<String>,
    ) -> Result<GetObjectResponse, Box<dyn APIError>> {
        match self.get_client() {
            Ok(client) => {
                let request = GetObjectRequest {
                    bucket: self.bucket.clone(),
                    key: format!("{}/{}", self.base_prefix, key),
                    range,
                    ..Default::default()
                };

                match client.get_object(request).await {
                    Ok(res) => {
                        let content_length = res.content_length.unwrap_or(0) as u64;
                        let content_type = res
                            .content_type
                            .unwrap_or("application/octet-stream".to_string());
                        let etag = res.e_tag.unwrap_or("".to_string());

                        let stream = res.body.unwrap();
                        let stream = GenericByteStream::from(stream);

                        Ok(GetObjectResponse {
                            content_length,
                            content_type,
                            etag,
                            last_modified: res
                                .last_modified
                                .unwrap_or_else(|| Utc::now().to_rfc2822()),
                            body: stream,
                        })
                    }
                    Err(e) => Err(Box::new(InternalServerError {
                        message: format!("Internal Server Error"),
                    })),
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn put_object(
        &self,
        key: String,
        bytes: Bytes,
        content_type: Option<String>,
    ) -> Result<(), Box<dyn APIError>> {
        match self.get_client() {
            Ok(client) => {
                let request = PutObjectRequest {
                    bucket: self.bucket.clone(),
                    key: format!("{}/{}", self.base_prefix, key),
                    body: Some(bytes.to_vec().into()),
                    content_type,
                    ..Default::default()
                };

                match client.put_object(request).await {
                    Ok(_) => Ok(()),
                    Err(e) => Err(Box::new(InternalServerError {
                        message: format!("Internal Server Error"),
                    })),
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn create_multipart_upload(
        &self,
        key: String,
        content_type: Option<String>,
    ) -> Result<CreateMultipartUploadResponse, Box<dyn APIError>> {
        match self.get_client() {
            Ok(client) => {
                let request = CreateMultipartUploadRequest {
                    bucket: self.bucket.clone(),
                    key: format!("{}/{}", self.base_prefix, key),
                    content_type,
                    ..Default::default()
                };

                match client.create_multipart_upload(request).await {
                    Ok(result) => Ok(CreateMultipartUploadResponse {
                        bucket: self.account_id.clone(),
                        key: key.clone(),
                        upload_id: result.upload_id.unwrap(),
                    }),
                    Err(e) => Err(Box::new(InternalServerError {
                        message: format!("Internal Server Error"),
                    })),
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn abort_multipart_upload(
        &self,
        key: String,
        upload_id: String,
    ) -> Result<(), Box<dyn APIError>> {
        match self.get_client() {
            Ok(client) => {
                let request = AbortMultipartUploadRequest {
                    bucket: self.bucket.clone(),
                    key: format!("{}/{}", self.base_prefix, key),
                    upload_id,
                    ..Default::default()
                };

                match client.abort_multipart_upload(request).await {
                    Ok(_) => Ok(()),
                    Err(_) => Err(Box::new(InternalServerError {
                        message: format!("Internal Server Error"),
                    })),
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn complete_multipart_upload(
        &self,
        key: String,
        upload_id: String,
        parts: Vec<MultipartPart>,
    ) -> Result<CompleteMultipartUploadResponse, Box<dyn APIError>> {
        match self.get_client() {
            Ok(client) => {
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

                match client.complete_multipart_upload(request).await {
                    Ok(result) => Ok(CompleteMultipartUploadResponse {
                        location: "".to_string(),
                        bucket: self.account_id.clone(),
                        key: key.clone(),
                        etag: result.e_tag.unwrap(),
                    }),
                    Err(e) => Err(Box::new(InternalServerError {
                        message: format!("Internal Server Error"),
                    })),
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn upload_multipart_part(
        &self,
        key: String,
        upload_id: String,
        part_number: String,
        bytes: Bytes,
    ) -> Result<UploadPartResponse, Box<dyn APIError>> {
        match self.get_client() {
            Ok(client) => {
                let request = UploadPartRequest {
                    bucket: self.bucket.clone(),
                    key: format!("{}/{}", self.base_prefix, key),
                    upload_id,
                    part_number: part_number.parse().unwrap(),
                    body: Some(bytes.to_vec().into()),
                    ..Default::default()
                };

                match client.upload_part(request).await {
                    Ok(result) => Ok(UploadPartResponse {
                        etag: result.e_tag.unwrap(),
                    }),
                    Err(_) => Err(Box::new(InternalServerError {
                        message: format!("Internal Server Error"),
                    })),
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn delete_object(&self, key: String) -> Result<(), Box<dyn APIError>> {
        match self.get_client() {
            Ok(client) => {
                let request = DeleteObjectRequest {
                    bucket: self.bucket.clone(),
                    key: format!("{}/{}", self.base_prefix, key),
                    ..Default::default()
                };

                match client.delete_object(request).await {
                    Ok(_) => Ok(()),
                    Err(_) => Err(Box::new(InternalServerError {
                        message: format!("Internal Server Error"),
                    })),
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn head_object(&self, key: String) -> Result<HeadObjectResponse, Box<dyn APIError>> {
        match self.get_client() {
            Ok(client) => {
                let request = HeadObjectRequest {
                    bucket: self.bucket.clone(),
                    key: format!("{}/{}", self.base_prefix, key),
                    ..Default::default()
                };

                match client.head_object(request).await {
                    Ok(result) => Ok(HeadObjectResponse {
                        content_length: result.content_length.unwrap_or(0) as u64,
                        content_type: result.content_type.unwrap_or_else(|| "".to_string()),
                        etag: result.e_tag.unwrap_or_else(|| "".to_string()),
                        last_modified: result
                            .last_modified
                            .unwrap_or_else(|| Utc::now().to_rfc2822()),
                    }),
                    Err(error) => {
                        error!("Error: {:?}", &error);
                        match error {
                            RusotoError::Unknown(response) => {
                                if response.status.eq(&404) {
                                    return Err(Box::new(ObjectNotFoundError {
                                        account_id: self.account_id.clone(),
                                        repository_id: self.repository_id.clone(),
                                        key,
                                    }));
                                }
                            }
                            _ => (),
                        }

                        Err(Box::new(InternalServerError {
                            message: format!("Internal Server Error"),
                        }))
                    }
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn list_objects_v2(
        &self,
        prefix: String,
        continuation_token: Option<String>,
        delimiter: Option<String>,
        max_keys: NonZeroU32,
    ) -> Result<ListBucketResult, Box<dyn APIError>> {
        match self.get_client() {
            Ok(client) => {
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

                match client.list_objects_v2(request).await {
                    Ok(output) => {
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
                                    storage_class: item
                                        .storage_class
                                        .clone()
                                        .unwrap_or_else(|| "".to_string()),
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

                        return Ok(result);
                    }
                    Err(_) => {
                        return Err(Box::new(InternalServerError {
                            message: "Internal Server Error".to_string(),
                        }));
                    }
                }
            }
            Err(e) => Err(e),
        }
    }
}

impl fmt::Debug for S3Repository {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Repository")
            .field("account_id", &self.account_id)
            .field("repository_id", &self.repository_id)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("base_prefix", &self.base_prefix)
            .field("auth_method", &self.auth_method)
            .field(
                "access_key_id",
                &self.access_key_id.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "secret_access_key",
                &self.secret_access_key.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}
