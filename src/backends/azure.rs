use actix_web::http::header::RANGE;
use async_trait::async_trait;
use azure_core::request_options::NextMarker;
use azure_storage::StorageCredentials;
use azure_storage_blobs::container::operations::list_blobs::BlobItem;
use azure_storage_blobs::prelude::*;
use bytes::Bytes;
use core::num::NonZeroU32;
use futures::StreamExt;
use futures_core::Stream;
use reqwest;
use std::fmt;
use std::pin::Pin;
use time::format_description::well_known::{Rfc2822, Rfc3339};

use crate::backends::common::{
    CommonPrefix, CompleteMultipartUploadResponse, Content, CreateMultipartUploadResponse,
    GetObjectResponse, HeadObjectResponse, ListBucketResult, Repository,
};
use crate::utils::core::{replace_first, GenericByteStream};
use crate::utils::errors::{APIError, InternalServerError, ObjectNotFoundError};

use super::common::{MultipartPart, UploadPartResponse};

pub struct AzureRepository {
    pub account_id: String,
    pub repository_id: String,
    pub account_name: String,
    pub container_name: String,
    pub base_prefix: String,
}

use chrono::format::strftime::StrftimeItems;
use chrono::{DateTime, FixedOffset};

fn rfc2822_to_rfc7231(rfc2822_date: &str) -> Result<String, chrono::ParseError> {
    // Parse the RFC2822 date string
    let datetime = DateTime::parse_from_rfc2822(rfc2822_date)?;

    // Define the format string for RFC7231
    let format = StrftimeItems::new("%a, %d %b %Y %H:%M:%S GMT");

    // Convert to UTC and format as RFC7231
    Ok(datetime
        .with_timezone(&FixedOffset::east_opt(0).unwrap())
        .format_with_items(format.clone())
        .to_string())
}

#[async_trait]
impl Repository for AzureRepository {
    async fn get_object(
        &self,
        key: String,
        range: Option<String>,
    ) -> Result<GetObjectResponse, Box<dyn APIError>> {
        let credentials = StorageCredentials::anonymous();

        let client = BlobServiceClient::new(format!("{}", &self.account_name), credentials)
            .container_client(&self.container_name);

        let blob_client = client.blob_client(format!(
            "{}/{}",
            self.base_prefix.trim_end_matches('/').to_string(),
            key
        ));

        match blob_client.get_properties().await {
            Ok(blob) => {
                let content_type = blob.blob.properties.content_type.to_string();
                let etag = blob.blob.properties.etag.to_string();
                let last_modified = rfc2822_to_rfc7231(
                    blob.blob
                        .properties
                        .last_modified
                        .format(&Rfc2822)
                        .unwrap_or_else(|_| String::from("Invalid DateTime"))
                        .as_str(),
                )
                .unwrap_or_else(|_| String::from("Invalid DateTime"));

                let client = reqwest::Client::new();

                // Start building the request
                let mut request = client.get(format!(
                    "https://{}.blob.core.windows.net/{}/{}/{}",
                    self.account_name,
                    self.container_name,
                    self.base_prefix.trim_end_matches('/').to_string(),
                    key
                ));

                // If a range is provided, add it to the headers
                if let Some(range_value) = range {
                    request = request.header(RANGE, range_value);
                }

                // Send the request and await the response
                match request.send().await {
                    Ok(response) => {
                        // Check if the status code is successful
                        if !response.status().is_success() {
                            return Err(Box::new(InternalServerError {
                                message: "Internal Server Error".to_string(),
                            }));
                        }

                        // Get the byte stream from the response
                        let content_length = response.content_length();
                        let generic_stream = GenericByteStream::from(response);

                        Ok(GetObjectResponse {
                            content_length: content_length.unwrap_or(0) as u64,
                            content_type,
                            etag,
                            last_modified,
                            body: generic_stream,
                        })
                    }
                    Err(_) => Err(Box::new(InternalServerError {
                        message: "Internal Server Error".to_string(),
                    })),
                }
            }
            Err(_) => Err(Box::new(InternalServerError {
                message: "Internal Server Error".to_string(),
            })),
        }
    }

    async fn delete_object(&self, _key: String) -> Result<(), Box<dyn APIError>> {
        Err(Box::new(InternalServerError {
            message: "Internal Server Error".to_string(),
        }))
    }

    async fn create_multipart_upload(
        &self,
        _key: String,
        _content_type: Option<String>,
    ) -> Result<CreateMultipartUploadResponse, Box<dyn APIError>> {
        Err(Box::new(InternalServerError {
            message: format!("Internal Server Error"),
        }))
    }

    async fn abort_multipart_upload(
        &self,
        _key: String,
        _upload_id: String,
    ) -> Result<(), Box<dyn APIError>> {
        Err(Box::new(InternalServerError {
            message: format!("Internal Server Error"),
        }))
    }

    async fn complete_multipart_upload(
        &self,
        _key: String,
        _upload_id: String,
        _parts: Vec<MultipartPart>,
    ) -> Result<CompleteMultipartUploadResponse, Box<dyn APIError>> {
        Err(Box::new(InternalServerError {
            message: format!("Internal Server Error"),
        }))
    }

    async fn upload_multipart_part(
        &self,
        _key: String,
        _upload_id: String,
        _part_number: String,
        _bytes: Bytes,
    ) -> Result<UploadPartResponse, Box<dyn APIError>> {
        Err(Box::new(InternalServerError {
            message: format!("Internal Server Error"),
        }))
    }

    async fn put_object(
        &self,
        _key: String,
        _bytes: Bytes,
        _content_type: Option<String>,
    ) -> Result<(), Box<dyn APIError>> {
        Err(Box::new(InternalServerError {
            message: "Internal Server Error".to_string(),
        }))
    }

    async fn head_object(&self, key: String) -> Result<HeadObjectResponse, Box<dyn APIError>> {
        let credentials = StorageCredentials::anonymous();

        // Create a client for anonymous access
        let client = BlobServiceClient::new(format!("{}", &self.account_name), credentials)
            .container_client(&self.container_name);

        match client
            .blob_client(format!(
                "{}/{}",
                self.base_prefix.trim_end_matches('/').to_string(),
                key
            ))
            .get_properties()
            .await
        {
            Ok(blob) => Ok(HeadObjectResponse {
                content_length: blob.blob.properties.content_length,
                content_type: blob.blob.properties.content_type.to_string(),
                etag: blob.blob.properties.etag.to_string(),
                last_modified: rfc2822_to_rfc7231(
                    blob.blob
                        .properties
                        .last_modified
                        .format(&Rfc2822)
                        .unwrap_or_else(|_| String::from("Invalid DateTime"))
                        .as_str(),
                )
                .unwrap_or_else(|_| String::from("Invalid DateTime")),
            }),
            Err(e) => {
                if e.as_http_error().unwrap().status() == 404 {
                    return Err(Box::new(ObjectNotFoundError {
                        account_id: self.account_id.clone(),
                        repository_id: self.repository_id.clone(),
                        key,
                    }));
                } else {
                    Err(Box::new(InternalServerError {
                        message: "Internal Server Error".to_string(),
                    }))
                }
            }
        }
    }

    async fn list_objects_v2(
        &self,
        prefix: String,
        continuation_token: Option<String>,
        delimiter: Option<String>,
        max_keys: NonZeroU32,
    ) -> Result<ListBucketResult, Box<dyn APIError>> {
        let mut result = ListBucketResult {
            name: format!("{}", self.account_id),
            prefix: prefix.clone(),
            key_count: 0,
            max_keys: 0,
            is_truncated: false,
            contents: vec![],
            common_prefixes: vec![],
            next_continuation_token: None,
        };

        let credentials = StorageCredentials::anonymous();

        // Create a client for anonymous access
        let client = BlobServiceClient::new(format!("{}", &self.account_name), credentials)
            .container_client(&self.container_name);
        let search_prefix = format!("{}/{}", self.base_prefix.trim_end_matches('/'), prefix);

        let next_marker = continuation_token.map_or(NextMarker::new("".to_string()), Into::into);

        let query_delmiter = delimiter.unwrap_or_else(|| "".to_string());

        // List blobs
        let mut stream = client
            .list_blobs()
            .marker(next_marker)
            .prefix(search_prefix)
            .max_results(max_keys)
            .delimiter(query_delmiter)
            .into_stream();

        if let Some(blob_result) = stream.next().await {
            match blob_result {
                Ok(blob) => {
                    if blob.max_results.is_some() {
                        result.max_keys = blob.max_results.unwrap() as i64;
                    }

                    if blob.next_marker.is_some() {
                        result.is_truncated = true;
                        result.next_continuation_token = Some(
                            blob.next_marker
                                .unwrap_or(NextMarker::new("".to_string()))
                                .as_str()
                                .to_string(),
                        );
                    }

                    for blob_item in blob.blobs.items {
                        match blob_item {
                            BlobItem::Blob(b) => {
                                result.contents.push(Content {
                                    key: replace_first(
                                        b.name,
                                        self.base_prefix.clone().trim_end_matches('/').to_string(),
                                        format!("{}", self.repository_id),
                                    ),
                                    last_modified: b
                                        .properties
                                        .last_modified
                                        .format(&Rfc3339)
                                        .unwrap_or_else(|_| String::from("Invalid DateTime")),
                                    etag: b.properties.etag.to_string(),
                                    size: b.properties.content_length as i64,
                                    storage_class: b.properties.blob_type.to_string(),
                                });
                            }
                            BlobItem::BlobPrefix(bp) => {
                                result.common_prefixes.push(CommonPrefix {
                                    prefix: replace_first(
                                        bp.name,
                                        self.base_prefix.clone().trim_end_matches('/').to_string(),
                                        format!("{}", self.repository_id),
                                    ),
                                });
                            }
                        }
                    }
                }
                Err(_) => (),
            }
        }

        Ok(result)
    }
}

impl fmt::Debug for AzureRepository {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AzureRepository")
            .field("account_id", &self.account_id)
            .field("repository_id", &self.repository_id)
            .field("account_name", &self.account_name)
            .field("container_name", &self.container_name)
            .field("base_prefix", &self.base_prefix)
            .finish()
    }
}
