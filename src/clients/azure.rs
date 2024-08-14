use actix_web::http::header::RANGE;
use async_trait::async_trait;
use azure_storage::StorageCredentials;
use azure_storage_blobs::container::operations::list_blobs::BlobItem;
use azure_storage_blobs::prelude::*;
use bytes::Bytes;
use futures::StreamExt;
use futures_core::Stream;
use reqwest;
use std::pin::Pin;
use time::format_description::well_known::Rfc2822;

use crate::clients::common::{
    CommonPrefix, Content, GetObjectResponse, HeadObjectResponse, ListBucketResult, Repository,
};
use crate::utils::core::replace_first;

// TODO: Polish ListObjects, Implement Markers, Max Size, etc. Implement GetObject with Range Request, Implement HeadObject

pub struct AzureRepository {
    pub account_id: String,
    pub repository_id: String,
    pub account_name: String,
    pub container_name: String,
    pub base_prefix: String,
    pub delimiter: String,
}

#[async_trait]
impl Repository for AzureRepository {
    async fn get_object(
        &self,
        key: String,
        range: Option<String>,
    ) -> Result<GetObjectResponse, ()> {
        let credentials = StorageCredentials::anonymous();

        let client = BlobServiceClient::new(format!("{}", &self.account_name), credentials)
            .container_client(&self.container_name);

        let blob_client = client.blob_client(format!("{}/{}", self.base_prefix, key));

        match blob_client.get_properties().await {
            Ok(blob) => {
                let content_type = blob.blob.properties.content_type.to_string();
                let etag = blob.blob.properties.etag.to_string();
                let last_modified = blob
                    .blob
                    .properties
                    .last_modified
                    .format(&Rfc2822)
                    .unwrap_or_else(|_| String::from("Invalid DateTime"));

                let client = reqwest::Client::new();

                // Start building the request
                let mut request = client.get(format!(
                    "https://{}.blob.core.windows.net/{}/{}/{}",
                    self.account_name, self.container_name, self.base_prefix, key
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
                            content_type,
                            etag,
                            last_modified,
                            body: boxed_stream,
                        })
                    }
                    Err(_) => Err(()),
                }
            }
            Err(_) => Err(()),
        }
    }

    async fn head_object(&self, key: String) -> Result<HeadObjectResponse, ()> {
        let credentials = StorageCredentials::anonymous();

        // Create a client for anonymous access
        let client = BlobServiceClient::new(format!("{}", &self.account_name), credentials)
            .container_client(&self.container_name);

        match client
            .blob_client(format!("{}/{}", self.base_prefix, key))
            .get_properties()
            .await
        {
            Ok(blob) => Ok(HeadObjectResponse {
                content_length: blob.blob.properties.content_length,
                content_type: blob.blob.properties.content_type.to_string(),
                etag: blob.blob.properties.etag.to_string(),
                last_modified: blob
                    .blob
                    .properties
                    .last_modified
                    .format(&Rfc2822)
                    .unwrap_or_else(|_| String::from("Invalid DateTime")),
            }),
            Err(_) => Err(()),
        }
    }

    async fn list_objects_v2(&self, prefix: String) -> Result<ListBucketResult, ()> {
        let mut result = ListBucketResult {
            name: format!("{}", self.account_id),
            prefix: prefix.clone(),
            key_count: 0,
            max_keys: 0,
            is_truncated: false,
            contents: vec![],
            common_prefixes: vec![],
        };

        let delimiter = self.delimiter.clone();

        let credentials = StorageCredentials::anonymous();

        // Create a client for anonymous access
        let client = BlobServiceClient::new(format!("{}", &self.account_name), credentials)
            .container_client(&self.container_name);

        let search_prefix = format!("{}/{}", self.base_prefix, prefix);

        // List blobs
        let mut stream = client
            .list_blobs()
            .prefix(search_prefix)
            .delimiter(delimiter)
            .into_stream();

        while let Some(blob_result) = stream.next().await {
            match blob_result {
                Ok(blob) => {
                    for blob_item in blob.blobs.items {
                        match blob_item {
                            BlobItem::Blob(b) => {
                                result.contents.push(Content {
                                    key: replace_first(
                                        b.name,
                                        self.base_prefix.clone(),
                                        format!("{}", self.repository_id),
                                    ),
                                    last_modified: b
                                        .properties
                                        .last_modified
                                        .format(&Rfc2822)
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
                                        self.base_prefix.clone(),
                                        format!("{}", self.repository_id),
                                    ),
                                });
                            }
                        }
                    }
                }
                Err(e) => eprintln!("Error listing blob: {:?}", e),
            }
        }

        Ok(result)
    }
}
