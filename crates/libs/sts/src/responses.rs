//! STS XML response serialization.

use quick_xml::se::to_string as xml_to_string;
use serde::Serialize;

/// STS AssumeRoleWithWebIdentity response.
#[derive(Debug, Serialize)]
#[serde(rename = "AssumeRoleWithWebIdentityResponse")]
pub struct AssumeRoleWithWebIdentityResponse {
    #[serde(rename = "AssumeRoleWithWebIdentityResult")]
    pub result: AssumeRoleWithWebIdentityResult,
}

#[derive(Debug, Serialize)]
pub struct AssumeRoleWithWebIdentityResult {
    #[serde(rename = "Credentials")]
    pub credentials: StsCredentials,
    #[serde(rename = "AssumedRoleUser")]
    pub assumed_role_user: AssumedRoleUser,
}

#[derive(Debug, Serialize)]
pub struct StsCredentials {
    #[serde(rename = "AccessKeyId")]
    pub access_key_id: String,
    #[serde(rename = "SecretAccessKey")]
    pub secret_access_key: String,
    #[serde(rename = "SessionToken")]
    pub session_token: String,
    #[serde(rename = "Expiration")]
    pub expiration: String,
}

#[derive(Debug, Serialize)]
pub struct AssumedRoleUser {
    #[serde(rename = "AssumedRoleId")]
    pub assumed_role_id: String,
    #[serde(rename = "Arn")]
    pub arn: String,
}

impl AssumeRoleWithWebIdentityResponse {
    pub fn to_xml(&self) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{}",
            xml_to_string(self).unwrap_or_default()
        )
    }
}
