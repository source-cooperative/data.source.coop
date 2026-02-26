//! Azure AD federated token exchange.
//!
//! Exchanges a self-signed JWT for an Azure access token via the
//! OAuth 2.0 client credentials grant with federated identity.

use crate::{CloudCredentials, HttpExchange, OidcProviderError};

use super::CredentialExchange;

/// Configuration for exchanging a JWT for Azure credentials.
#[derive(Debug, Clone)]
pub struct AzureExchange {
    /// Azure AD tenant ID.
    pub tenant_id: String,

    /// Application (client) ID of the Azure AD app registration.
    pub client_id: String,

    /// The scope to request (e.g. `https://storage.azure.com/.default`).
    pub scope: String,
}

impl AzureExchange {
    pub fn new(tenant_id: String, client_id: String) -> Self {
        Self {
            tenant_id,
            client_id,
            scope: "https://storage.azure.com/.default".into(),
        }
    }

    pub fn with_scope(mut self, scope: String) -> Self {
        self.scope = scope;
        self
    }

    fn token_endpoint(&self) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            self.tenant_id
        )
    }
}

impl<H: HttpExchange> CredentialExchange<H> for AzureExchange {
    async fn exchange(&self, http: &H, jwt: &str) -> Result<CloudCredentials, OidcProviderError> {
        let form = [
            ("grant_type", "client_credentials"),
            (
                "client_assertion_type",
                "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
            ),
            ("client_assertion", jwt),
            ("client_id", &self.client_id),
            ("scope", &self.scope),
        ];

        let body = http.post_form(&self.token_endpoint(), &form).await?;

        parse_azure_token_response(&body)
    }
}

/// Parse an Azure AD token response.
fn parse_azure_token_response(json: &str) -> Result<CloudCredentials, OidcProviderError> {
    let parsed: serde_json::Value = serde_json::from_str(json).map_err(|e| {
        OidcProviderError::ExchangeError(format!("invalid Azure token response: {e}"))
    })?;

    if let Some(err) = parsed.get("error") {
        let desc = parsed
            .get("error_description")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Err(OidcProviderError::ExchangeError(format!(
            "Azure AD error: {err} — {desc}"
        )));
    }

    let access_token = parsed["access_token"]
        .as_str()
        .ok_or_else(|| OidcProviderError::ExchangeError("missing access_token".into()))?;

    let expires_in = parsed["expires_in"]
        .as_i64()
        .ok_or_else(|| OidcProviderError::ExchangeError("missing expires_in".into()))?;

    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(expires_in);

    // Azure returns a bearer token, not key/secret pair. We store it as the
    // session_token and use placeholder values for key_id/secret — the backend
    // will use the bearer token directly.
    Ok(CloudCredentials {
        access_key_id: String::new(),
        secret_access_key: String::new(),
        session_token: access_token.to_string(),
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_token_response() {
        let json = r#"{
            "access_token": "eyJ0eXAiOiJKV1Q...",
            "token_type": "Bearer",
            "expires_in": 3600
        }"#;

        let creds = parse_azure_token_response(json).unwrap();
        assert_eq!(creds.session_token, "eyJ0eXAiOiJKV1Q...");
        assert!(creds.expires_at > chrono::Utc::now());
    }

    #[test]
    fn parse_error_response() {
        let json = r#"{
            "error": "invalid_client",
            "error_description": "Client assertion failed"
        }"#;

        let err = parse_azure_token_response(json).unwrap_err();
        assert!(err.to_string().contains("Azure AD error"));
    }

    #[test]
    fn token_endpoint_format() {
        let ex = AzureExchange::new("tenant-123".into(), "client-456".into());
        assert_eq!(
            ex.token_endpoint(),
            "https://login.microsoftonline.com/tenant-123/oauth2/v2.0/token"
        );
    }
}
