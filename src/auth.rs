use multistore_oidc_provider::jwt::JwtSigner;

/// How the proxy authenticates to the Source Cooperative API.
#[derive(Clone)]
pub(crate) struct ApiAuth {
    signer: Box<JwtSigner>,
    issuer: String,
    audience: String,
}

impl ApiAuth {
    pub fn new(signer: JwtSigner, issuer: String, audience: String) -> Self {
        Self {
            signer: Box::new(signer),
            issuer,
            audience,
        }
    }

    pub fn authorization_header(&self, subject: &str) -> Option<String> {
        match self.signer.sign(subject, &self.issuer, &self.audience, &[]) {
            Ok(token) => Some(format!("Bearer {}", token)),
            Err(e) => {
                tracing::error!("failed to sign API auth JWT: {}", e);
                None
            }
        }
    }
}
