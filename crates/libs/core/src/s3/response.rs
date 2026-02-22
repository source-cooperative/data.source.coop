//! S3 XML response serialization.

use quick_xml::se::to_string as xml_to_string;
use serde::Serialize;

use crate::error::ProxyError;

/// S3 Error response XML.
#[derive(Debug, Serialize)]
#[serde(rename = "Error")]
pub struct ErrorResponse {
    #[serde(rename = "Code")]
    pub code: String,
    #[serde(rename = "Message")]
    pub message: String,
    #[serde(rename = "Resource")]
    pub resource: String,
    #[serde(rename = "RequestId")]
    pub request_id: String,
}

impl ErrorResponse {
    pub fn from_proxy_error(err: &ProxyError, resource: &str, request_id: &str) -> Self {
        Self {
            code: err.s3_error_code().to_string(),
            message: err.to_string(),
            resource: resource.to_string(),
            request_id: request_id.to_string(),
        }
    }

    pub fn to_xml(&self) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{}",
            xml_to_string(self)
                .unwrap_or_else(|_| "<Error><Code>InternalError</Code></Error>".to_string())
        )
    }
}

/// InitiateMultipartUpload response.
#[derive(Debug, Serialize)]
#[serde(rename = "InitiateMultipartUploadResult")]
pub struct InitiateMultipartUploadResult {
    #[serde(rename = "Bucket")]
    pub bucket: String,
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "UploadId")]
    pub upload_id: String,
}

impl InitiateMultipartUploadResult {
    pub fn to_xml(&self) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{}",
            xml_to_string(self).unwrap_or_default()
        )
    }
}

/// CompleteMultipartUpload response.
#[derive(Debug, Serialize)]
#[serde(rename = "CompleteMultipartUploadResult")]
pub struct CompleteMultipartUploadResult {
    #[serde(rename = "Location")]
    pub location: String,
    #[serde(rename = "Bucket")]
    pub bucket: String,
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "ETag")]
    pub etag: String,
}

impl CompleteMultipartUploadResult {
    pub fn to_xml(&self) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{}",
            xml_to_string(self).unwrap_or_default()
        )
    }
}

/// Request body for CompleteMultipartUpload.
#[derive(Debug, serde::Deserialize)]
#[serde(rename = "CompleteMultipartUpload")]
pub struct CompleteMultipartUploadRequest {
    #[serde(rename = "Part")]
    pub parts: Vec<CompletePart>,
}

#[derive(Debug, serde::Deserialize)]
pub struct CompletePart {
    #[serde(rename = "PartNumber")]
    pub part_number: u32,
    #[serde(rename = "ETag")]
    pub etag: String,
}

/// ListAllMyBucketsResult response (for `GET /`).
#[derive(Debug, Serialize)]
#[serde(rename = "ListAllMyBucketsResult")]
pub struct ListAllMyBucketsResult {
    #[serde(rename = "Owner")]
    pub owner: BucketOwner,
    #[serde(rename = "Buckets")]
    pub buckets: BucketList,
}

#[derive(Debug, Serialize)]
pub struct BucketOwner {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "DisplayName")]
    pub display_name: String,
}

#[derive(Debug, Serialize)]
pub struct BucketList {
    #[serde(rename = "Bucket")]
    pub buckets: Vec<BucketEntry>,
}

#[derive(Debug, Serialize)]
pub struct BucketEntry {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "CreationDate")]
    pub creation_date: String,
}

impl ListAllMyBucketsResult {
    pub fn to_xml(&self) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{}",
            xml_to_string(self).unwrap_or_default()
        )
    }
}

/// S3 ListObjectsV2 response.
#[derive(Debug, Serialize)]
#[serde(rename = "ListBucketResult")]
pub struct ListBucketResult {
    #[serde(rename = "@xmlns")]
    pub xmlns: &'static str,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Prefix")]
    pub prefix: String,
    #[serde(rename = "Delimiter")]
    pub delimiter: String,
    #[serde(rename = "MaxKeys")]
    pub max_keys: usize,
    #[serde(rename = "IsTruncated")]
    pub is_truncated: bool,
    #[serde(rename = "KeyCount")]
    pub key_count: usize,
    #[serde(rename = "Contents", default)]
    pub contents: Vec<ListContents>,
    #[serde(rename = "CommonPrefixes", default)]
    pub common_prefixes: Vec<ListCommonPrefix>,
}

#[derive(Debug, Serialize)]
pub struct ListContents {
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "LastModified")]
    pub last_modified: String,
    #[serde(rename = "ETag")]
    pub etag: String,
    #[serde(rename = "Size")]
    pub size: u64,
    #[serde(rename = "StorageClass")]
    pub storage_class: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ListCommonPrefix {
    #[serde(rename = "Prefix")]
    pub prefix: String,
}

impl ListBucketResult {
    pub fn to_xml(&self) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{}",
            xml_to_string(self).unwrap_or_default()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_bucket_result_xml() {
        let result = ListBucketResult {
            xmlns: "http://s3.amazonaws.com/doc/2006-03-01/",
            name: "my-bucket".to_string(),
            prefix: "photos/".to_string(),
            delimiter: "/".to_string(),
            max_keys: 1000,
            is_truncated: false,
            key_count: 1,
            contents: vec![ListContents {
                key: "photos/image.jpg".to_string(),
                last_modified: "2024-01-01T00:00:00.000Z".to_string(),
                etag: "\"abc123\"".to_string(),
                size: 1024,
                storage_class: "STANDARD",
            }],
            common_prefixes: vec![ListCommonPrefix {
                prefix: "photos/thumbs/".to_string(),
            }],
        };

        let xml = result.to_xml();
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(
            xml.contains("<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">")
        );
        assert!(xml.contains("<Name>my-bucket</Name>"));
        assert!(xml.contains("<Key>photos/image.jpg</Key>"));
        assert!(xml.contains("<Size>1024</Size>"));
        assert!(xml.contains("<CommonPrefixes><Prefix>photos/thumbs/</Prefix></CommonPrefixes>"));
    }

    #[test]
    fn test_list_bucket_result_empty() {
        let result = ListBucketResult {
            xmlns: "http://s3.amazonaws.com/doc/2006-03-01/",
            name: "bucket".to_string(),
            prefix: String::new(),
            delimiter: "/".to_string(),
            max_keys: 1000,
            is_truncated: false,
            key_count: 0,
            contents: vec![],
            common_prefixes: vec![],
        };

        let xml = result.to_xml();
        assert!(xml.contains("<KeyCount>0</KeyCount>"));
        assert!(!xml.contains("<Contents>"));
        assert!(!xml.contains("<CommonPrefixes>"));
    }
}
