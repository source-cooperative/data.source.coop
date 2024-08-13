use async_trait::async_trait;
use rusoto_core::Region;
use rusoto_s3::{ListObjectsV2Request, S3Client, S3};
use chrono::Utc;
use time::format_description::well_known::Rfc2822;

use crate::clients::common::{Content, CommonPrefix, ListBucketResult, Repository};

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
                    prefix: prefix.clone(),
                    key_count: output.key_count.unwrap_or(0),
                    max_keys: output.max_keys.unwrap_or(0),
                    is_truncated: output.is_truncated.unwrap_or(false),
                    contents: output.contents.unwrap_or_default().iter().map(|item| {
                        Content {
                            key: item.key.clone().unwrap_or_else(|| "".to_string()),
                            last_modified: item.last_modified.clone().unwrap_or_else(|| Utc::now().to_rfc2822()),
                            etag: item.e_tag.clone().unwrap_or_else(|| "".to_string()),
                            size: item.size.unwrap_or(0),
                            storage_class: item.storage_class.clone().unwrap_or_else(|| "".to_string()),
                        }
                    }).collect(),
                    common_prefixes: output.common_prefixes.unwrap_or_default().iter().map(|item| {
                        CommonPrefix {
                            prefix: item.prefix.clone().unwrap_or_else(|| "".to_string())
                        }
                    }).collect(),
                };

                return Ok(result);
            }
            Err(_) => {
                return Err(());
            }
        }
    }
}
