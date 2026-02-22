//! AWS STS `AssumeRoleWithWebIdentity` credential exchange.

use crate::{CloudCredentials, HttpExchange, OidcProviderError};

use super::CredentialExchange;

/// Configuration for exchanging a JWT for AWS credentials.
#[derive(Debug, Clone)]
pub struct AwsExchange {
    /// The ARN of the IAM role to assume (e.g. `arn:aws:iam::123456789012:role/MyRole`).
    pub role_arn: String,

    /// AWS STS endpoint. Defaults to the global endpoint.
    pub sts_endpoint: String,

    /// Session name included in the assumed role credentials.
    pub session_name: String,
}

impl Default for AwsExchange {
    fn default() -> Self {
        Self {
            role_arn: String::new(),
            sts_endpoint: "https://sts.amazonaws.com".into(),
            session_name: "s3-proxy".into(),
        }
    }
}

impl AwsExchange {
    pub fn new(role_arn: String) -> Self {
        Self {
            role_arn,
            ..Default::default()
        }
    }

    pub fn with_endpoint(mut self, endpoint: String) -> Self {
        self.sts_endpoint = endpoint;
        self
    }

    pub fn with_session_name(mut self, name: String) -> Self {
        self.session_name = name;
        self
    }
}

impl<H: HttpExchange> CredentialExchange<H> for AwsExchange {
    async fn exchange(
        &self,
        http: &H,
        jwt: &str,
    ) -> Result<CloudCredentials, OidcProviderError> {
        let form = [
            ("Action", "AssumeRoleWithWebIdentity"),
            ("Version", "2011-06-15"),
            ("RoleArn", &self.role_arn),
            ("RoleSessionName", &self.session_name),
            ("WebIdentityToken", jwt),
        ];

        let body = http
            .post_form(&self.sts_endpoint, &form)
            .await?;

        parse_assume_role_response(&body)
    }
}

/// Parse the XML response from AWS STS `AssumeRoleWithWebIdentity`.
fn parse_assume_role_response(xml: &str) -> Result<CloudCredentials, OidcProviderError> {
    // Extract fields from the STS XML response.
    // The response structure is:
    // <AssumeRoleWithWebIdentityResponse>
    //   <AssumeRoleWithWebIdentityResult>
    //     <Credentials>
    //       <AccessKeyId>...</AccessKeyId>
    //       <SecretAccessKey>...</SecretAccessKey>
    //       <SessionToken>...</SessionToken>
    //       <Expiration>...</Expiration>
    //     </Credentials>
    //   </AssumeRoleWithWebIdentityResult>
    // </AssumeRoleWithWebIdentityResponse>
    let access_key_id = extract_xml_value(xml, "AccessKeyId")?;
    let secret_access_key = extract_xml_value(xml, "SecretAccessKey")?;
    let session_token = extract_xml_value(xml, "SessionToken")?;
    let expiration_str = extract_xml_value(xml, "Expiration")?;

    let expires_at = chrono::DateTime::parse_from_rfc3339(&expiration_str)
        .map_err(|e| OidcProviderError::ExchangeError(format!("invalid Expiration: {e}")))?
        .with_timezone(&chrono::Utc);

    Ok(CloudCredentials {
        access_key_id,
        secret_access_key,
        session_token,
        expires_at,
    })
}

/// Simple XML tag value extraction (avoids pulling in a full XML parser).
fn extract_xml_value(xml: &str, tag: &str) -> Result<String, OidcProviderError> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml
        .find(&open)
        .ok_or_else(|| OidcProviderError::ExchangeError(format!("missing <{tag}> in STS response")))?
        + open.len();
    let end = xml[start..]
        .find(&close)
        .ok_or_else(|| OidcProviderError::ExchangeError(format!("missing </{tag}> in STS response")))?
        + start;
    Ok(xml[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sts_response() {
        let xml = r#"
<AssumeRoleWithWebIdentityResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
  <AssumeRoleWithWebIdentityResult>
    <Credentials>
      <AccessKeyId>ASIATESTKEYID</AccessKeyId>
      <SecretAccessKey>testsecretkey</SecretAccessKey>
      <SessionToken>testsessiontoken</SessionToken>
      <Expiration>2025-01-15T12:00:00Z</Expiration>
    </Credentials>
  </AssumeRoleWithWebIdentityResult>
</AssumeRoleWithWebIdentityResponse>"#;

        let creds = parse_assume_role_response(xml).unwrap();
        assert_eq!(creds.access_key_id, "ASIATESTKEYID");
        assert_eq!(creds.secret_access_key, "testsecretkey");
        assert_eq!(creds.session_token, "testsessiontoken");
        assert_eq!(creds.expires_at.to_rfc3339(), "2025-01-15T12:00:00+00:00");
    }

    #[test]
    fn parse_sts_response_missing_field() {
        let xml = "<Credentials><AccessKeyId>AK</AccessKeyId></Credentials>";
        let err = parse_assume_role_response(xml).unwrap_err();
        assert!(err.to_string().contains("SecretAccessKey"));
    }
}
