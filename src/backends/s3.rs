use crate::backends::common::{
    CommonPrefix, Content, GetObjectResponse, HeadObjectResponse, ListBucketResult, Repository,
};
use crate::utils::core::replace_first;
use crate::utils::errors::{APIError, InternalServerError, ObjectNotFoundError};
use actix_web::http::header::RANGE;
use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use core::num::NonZeroU32;
use futures_core::Stream;
use reqwest;
use rusoto_core::Region;
use rusoto_core::RusotoError;
use rusoto_s3::{HeadObjectRequest, ListObjectsV2Request, S3Client, S3};
use std::pin::Pin;

pub struct S3Repository {
    pub account_id: String,
    pub repository_id: String,
    pub region: Region,
    pub bucket: String,
    pub base_prefix: String,
    pub delimiter: String,
}

#[async_trait]
impl Repository for S3Repository {
    async fn get_object(
        &self,
        key: String,
        range: Option<String>,
    ) -> Result<GetObjectResponse, Box<dyn APIError>> {
        match self.head_object(key.clone()).await {
            Ok(head_object_response) => {
                let client = reqwest::Client::new();
                let url = format!(
                    "https://s3.{}.amazonaws.com/{}/{}/{}",
                    self.region.name(),
                    self.bucket,
                    self.base_prefix,
                    key
                );
                // Start building the request
                let mut request = client.get(url);

                // If a range is provided, add it to the headers
                if let Some(range_value) = range {
                    request = request.header(RANGE, range_value);
                }

                // Send the request and await the response
                match request.send().await {
                    Ok(response) => {
                        // Get the byte stream from the response
                        let content_length = response.content_length();
                        let stream = response.bytes_stream();
                        let boxed_stream: Pin<
                            Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>,
                        > = Box::pin(stream);

                        Ok(GetObjectResponse {
                            content_length: content_length.unwrap_or(0) as u64,
                            content_type: head_object_response.content_type,
                            etag: head_object_response.etag,
                            last_modified: head_object_response.last_modified,
                            body: boxed_stream,
                        })
                    }
                    Err(error) => {
                        if error.is_status() {
                            let code = error.status().unwrap().as_u16();
                            if code == 404 {
                                return Err(Box::new(ObjectNotFoundError {
                                    account_id: self.account_id.clone(),
                                    repository_id: self.repository_id.clone(),
                                    key,
                                }));
                            }
                        }

                        return Err(Box::new(InternalServerError {
                            message: "Internal Server Error".to_string(),
                        }));
                    }
                }
            }
            Err(err) => {
                // Pass through the error from the head_object call
                return Err(err);
            }
        }
    }

    async fn head_object(&self, key: String) -> Result<HeadObjectResponse, Box<dyn APIError>> {
        let client = S3Client::new(self.region.clone());
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

    async fn list_objects_v2(
        &self,
        prefix: String,
        continuation_token: Option<String>,
        max_keys: NonZeroU32,
    ) -> Result<ListBucketResult, Box<dyn APIError>> {
        let client = S3Client::new(self.region.clone());
        let mut request = ListObjectsV2Request {
            bucket: self.bucket.clone(),
            prefix: Some(format!("{}/{}", self.base_prefix, prefix)),
            delimiter: Some(self.delimiter.clone()),
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
            Err(error) => {
                dbg!(&error);
                return Err(Box::new(InternalServerError {
                    message: "Internal Server Error".to_string(),
                }));
            }
        }
    }
}
