//! Regression tests for issue #180: multipart backend URLs must
//! percent-encode the object key with the SigV4 strict set, or AWS/MinIO
//! reconstruct a different canonical URI and reject the request with
//! `SignatureDoesNotMatch`. Fixed upstream in multistore 0.6.4
//! (developmentseed/multistore#105); pinned here so a future dependency
//! change can't silently reintroduce it.

use std::collections::HashMap;

use multistore::backend::multipart::build_backend_url;
use multistore::types::{BucketConfig, S3Operation};

fn config() -> BucketConfig {
    BucketConfig {
        name: "virtual".into(),
        backend_type: "s3".into(),
        backend_prefix: None,
        anonymous_access: false,
        allowed_roles: vec![],
        backend_options: HashMap::from([
            (
                "endpoint".to_string(),
                "https://s3.us-west-2.amazonaws.com".to_string(),
            ),
            ("bucket_name".to_string(), "real-bucket".to_string()),
        ]),
    }
}

#[test]
fn create_multipart_upload_encodes_equals_in_key() {
    // The Hive-style partition key from issue #180.
    let op = S3Operation::CreateMultipartUpload {
        bucket: "virtual".into(),
        key: "by_country/country_iso=ETH/ETH.pmtiles".into(),
    };
    let url = build_backend_url(&config(), &op).unwrap();
    assert_eq!(
        url,
        "https://s3.us-west-2.amazonaws.com/real-bucket/by_country/country_iso%3DETH/ETH.pmtiles?uploads"
    );
}

#[test]
fn upload_part_strict_encodes_key_and_keeps_slashes() {
    let op = S3Operation::UploadPart {
        bucket: "virtual".into(),
        key: "dir/a key+(v2)=final*&#.bin".into(),
        upload_id: "uid123".into(),
        part_number: 2,
    };
    let url = build_backend_url(&config(), &op).unwrap();
    // `/` stays raw as the segment separator; everything outside the RFC 3986
    // unreserved set is percent-encoded.
    assert_eq!(
        url,
        "https://s3.us-west-2.amazonaws.com/real-bucket/dir/a%20key%2B%28v2%29%3Dfinal%2A%26%23.bin?partNumber=2&uploadId=uid123"
    );
}
