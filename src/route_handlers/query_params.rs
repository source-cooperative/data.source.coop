use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct GetObjectQuery {}

#[derive(Debug, Deserialize)]
pub struct AbortMultipartUploadQuery {
    #[serde(rename = "uploadId")]
    pub upload_id: String,
}

#[derive(Debug, Deserialize)]
pub struct UploadPartQuery {
    #[serde(rename = "partNumber")]
    pub part_number: String,
    #[serde(rename = "uploadId")]
    pub upload_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateMultipartUploadQuery {
    #[serde(rename = "uploads")]
    pub uploads: String,
}

#[derive(Debug, Deserialize)]
pub struct CompleteMultipartUploadQuery {
    #[serde(rename = "uploadId")]
    pub upload_id: String,
}
