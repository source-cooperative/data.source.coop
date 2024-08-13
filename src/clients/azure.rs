use async_trait::async_trait;
use azure_storage_blobs::container::operations::list_blobs::BlobItem;
use azure_storage_blobs::prelude::*;
use azure_storage::StorageCredentials;
use futures::StreamExt;
use time::format_description::well_known::Rfc2822;

use crate::clients::common::{Content, CommonPrefix, ListBucketResult, Repository};

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
        let client = BlobServiceClient::new(
            format!("{}", &self.account_name),
            credentials
        )
        .container_client(&self.container_name);

        let search_prefix = format!("{}/{}", self.base_prefix, prefix);
        dbg!(&self.container_name);
        dbg!(&search_prefix);
        dbg!(&delimiter);


        // List blobs
        let mut stream = client.list_blobs().prefix(search_prefix).delimiter(delimiter).into_stream();

        while let Some(blob_result) = stream.next().await {
            match blob_result {
                Ok(blob) => {
                    for blob_item in blob.blobs.items {
                        match blob_item {
                            BlobItem::Blob(b) => {
                                result.contents.push(Content {
                                    key: b.name,
                                    last_modified: b.properties.last_modified.format(&Rfc2822).unwrap_or_else(|_| String::from("Invalid DateTime")),
                                    etag: b.properties.etag.to_string(),
                                    size: b.properties.content_length as i64,
                                    storage_class: b.properties.blob_type.to_string(),
                                });
                            }
                            BlobItem::BlobPrefix(bp) => {
                                result.common_prefixes.push(CommonPrefix {
                                    prefix: bp.name,
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
