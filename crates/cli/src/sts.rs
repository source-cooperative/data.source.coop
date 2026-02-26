use quick_xml::de::from_str as xml_from_str;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: String,
    pub expiration: String,
}

/// Call the proxy's STS AssumeRoleWithWebIdentity endpoint.
pub async fn assume_role(
    proxy_url: &str,
    role_arn: &str,
    web_identity_token: &str,
    duration_seconds: Option<u64>,
) -> Result<Credentials, String> {
    let mut url = url::Url::parse(proxy_url)
        .map_err(|e| format!("Invalid proxy URL: {e}"))?;

    url.query_pairs_mut()
        .append_pair("Action", "AssumeRoleWithWebIdentity")
        .append_pair("RoleArn", role_arn)
        .append_pair("WebIdentityToken", web_identity_token);

    if let Some(duration) = duration_seconds {
        url.query_pairs_mut()
            .append_pair("DurationSeconds", &duration.to_string());
    }

    let resp = reqwest::get(url.as_str())
        .await
        .map_err(|e| format!("STS request failed: {e}"))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("Failed to read STS response: {e}"))?;

    if !status.is_success() {
        // Try to parse error XML for a better message
        if let Ok(err) = xml_from_str::<StsErrorResponse>(&body) {
            return Err(format!(
                "STS error ({}): {}",
                err.error.code, err.error.message
            ));
        }
        return Err(format!("STS request failed (HTTP {status}): {body}"));
    }

    let parsed: StsResponse =
        xml_from_str(&body).map_err(|e| format!("Failed to parse STS response XML: {e}"))?;

    let creds = parsed.result.credentials;
    Ok(Credentials {
        access_key_id: creds.access_key_id,
        secret_access_key: creds.secret_access_key,
        session_token: creds.session_token,
        expiration: creds.expiration,
    })
}

// XML deserialization types matching the STS response format

#[derive(Debug, Deserialize)]
#[serde(rename = "AssumeRoleWithWebIdentityResponse")]
struct StsResponse {
    #[serde(rename = "AssumeRoleWithWebIdentityResult")]
    result: StsResult,
}

#[derive(Debug, Deserialize)]
struct StsResult {
    #[serde(rename = "Credentials")]
    credentials: StsCredentials,
}

#[derive(Debug, Deserialize)]
struct StsCredentials {
    #[serde(rename = "AccessKeyId")]
    access_key_id: String,
    #[serde(rename = "SecretAccessKey")]
    secret_access_key: String,
    #[serde(rename = "SessionToken")]
    session_token: String,
    #[serde(rename = "Expiration")]
    expiration: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "ErrorResponse")]
struct StsErrorResponse {
    #[serde(rename = "Error")]
    error: StsError,
}

#[derive(Debug, Deserialize)]
struct StsError {
    #[serde(rename = "Code")]
    code: String,
    #[serde(rename = "Message")]
    message: String,
}
