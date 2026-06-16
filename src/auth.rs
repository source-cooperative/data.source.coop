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

    /// Build the `Authorization` header value for an API request on behalf of
    /// `subject`.
    ///
    /// Returns `None` if signing fails. The signing key is parsed and validated
    /// once at startup (`JwtSigner::from_pem`, which panics on a bad key), so a
    /// runtime failure here is very unlikely. When it does happen the error is
    /// logged and the caller falls through to an unauthenticated request, which
    /// the API surfaces as `AccessDenied` (403) rather than a 500.
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
