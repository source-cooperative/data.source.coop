//! STS XML response serialization.

use quick_xml::se::to_string as xml_to_string;
use source_coop_core::error::ProxyError;
use source_coop_core::types::TemporaryCredentials;
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

/// Build an STS success response (status code + XML body) from temporary credentials.
pub fn build_sts_response(creds: &TemporaryCredentials) -> (u16, String) {
    let response = AssumeRoleWithWebIdentityResponse {
        result: AssumeRoleWithWebIdentityResult {
            credentials: StsCredentials {
                access_key_id: creds.access_key_id.clone(),
                secret_access_key: creds.secret_access_key.clone(),
                session_token: creds.session_token.clone(),
                expiration: creds.expiration.to_rfc3339(),
            },
            assumed_role_user: AssumedRoleUser {
                assumed_role_id: creds.assumed_role_id.clone(),
                arn: creds.assumed_role_id.clone(),
            },
        },
    };
    (200, response.to_xml())
}

/// Build an STS error response (status code + XML body) from a ProxyError.
pub fn build_sts_error_response(err: &ProxyError) -> (u16, String) {
    let (status, code, message) = match err {
        ProxyError::RoleNotFound(r) => (400, "MalformedPolicyDocument", format!("role not found: {}", r)),
        ProxyError::InvalidOidcToken(msg) => (400, "InvalidIdentityToken", msg.clone()),
        ProxyError::InvalidRequest(msg) => (400, "InvalidParameterValue", msg.clone()),
        ProxyError::AccessDenied => (403, "AccessDenied", "access denied".to_string()),
        _ => (500, "InternalError", "internal error".to_string()),
    };

    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <ErrorResponse>\
           <Error>\
             <Code>{}</Code>\
             <Message>{}</Message>\
           </Error>\
         </ErrorResponse>",
        code, message
    );
    (status, xml)
}
