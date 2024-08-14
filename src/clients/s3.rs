use crate::clients::common::{
    CommonPrefix, Content, GetObjectResponse, HeadObjectResponse, ListBucketResult, Repository,
};
use crate::utils::core::replace_first;
use actix_web::http::header::RANGE;
use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use futures_core::Stream;
use reqwest;
use rusoto_core::Region;
use rusoto_s3::{HeadObjectRequest, ListObjectsV2Request, S3Client, S3};
use std::pin::Pin;
// TODO: Polish ListObject

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
    ) -> Result<GetObjectResponse, ()> {
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
                dbg!(&url);
                // Start building the request
                let mut request = client.get(url);

                // If a range is provided, add it to the headers
                if let Some(range_value) = range {
                    dbg!(&range_value);
                    request = request.header(RANGE, range_value);
                }

                // Send the request and await the response
                match request.send().await {
                    Ok(response) => {
                        // Check if the status code is successful
                        if !response.status().is_success() {
                            return Err(());
                        }

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
                    Err(_) => Err(()),
                }
            }
            Err(_) => return Err(()),
        }
    }

    async fn head_object(&self, key: String) -> Result<HeadObjectResponse, ()> {
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
            Err(_) => Err(()),
        }
    }

    async fn list_objects_v2(&self, prefix: String) -> Result<ListBucketResult, ()> {
        let client = S3Client::new(self.region.clone());
        let request = ListObjectsV2Request {
            bucket: self.bucket.clone(),
            prefix: Some(format!("{}/{}", self.base_prefix, prefix)),
            delimiter: Some(self.delimiter.clone()),
            ..Default::default()
        };

        match client.list_objects_v2(request).await {
            Ok(output) => {
                let result = ListBucketResult {
                    name: format!("{}", self.account_id),
                    prefix,
                    key_count: output.key_count.unwrap_or(0),
                    max_keys: output.max_keys.unwrap_or(0),
                    is_truncated: output.is_truncated.unwrap_or(false),
                    contents: output
                        .contents
                        .unwrap_or_default()
                        .iter()
                        .map(|item| Content {
                            key: replace_first(
                                item.key.clone().unwrap_or_else(|| "".to_string()),
                                self.base_prefix.clone(),
                                format!("{}/", self.repository_id),
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
                                format!("{}/", self.repository_id),
                            ),
                        })
                        .collect(),
                };

                return Ok(result);
            }
            Err(_) => {
                return Err(());
            }
        }
    }
}
