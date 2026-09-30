use multistore_oidc_provider::jwt::JwtSigner;

/// The subject the proxy signs with when it calls the API as itself rather
/// than on behalf of an account (ADR-013, amending ADR-005). Only the API-key
/// standing lookup accepts it: the API-key exchange happens before anything
/// names an account. A URN, so no account id can ever equal it, and refused
/// as an on-behalf-of subject so no request can claim it.
pub(crate) const PROXY_SELF_SUBJECT: &str = "urn:source:data-proxy";

/// Who an API request is made as.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ApiCaller<'a> {
    /// No credentials: the API answers as it would any stranger.
    Anonymous,
    /// On behalf of an account the proxy has authenticated.
    Account(&'a str),
    /// The proxy itself.
    Proxy,
}

impl<'a> From<Option<&'a str>> for ApiCaller<'a> {
    fn from(subject: Option<&'a str>) -> Self {
        subject.map_or(ApiCaller::Anonymous, ApiCaller::Account)
    }
}

/// How the proxy authenticates to the Source Cooperative API.
#[derive(Clone)]
pub(crate) struct ApiAuth {
    signer: JwtSigner,
    issuer: String,
    audience: String,
}

impl ApiAuth {
    pub fn new(signer: JwtSigner, issuer: String, audience: String) -> Self {
        Self {
            signer,
            issuer,
            audience,
        }
    }

    /// Build the `Authorization` header value for an API request on behalf of
    /// `subject`.
    ///
    /// Returns `None` if signing fails, or if `subject` is the proxy's own
    /// sentinel — that is `authorization_header_as_self`'s to sign, never a
    /// caller's to claim. The signing key is parsed and validated once at
    /// startup (`JwtSigner::from_pem`, which panics on a bad key), so a
    /// runtime signing failure is very unlikely. When it does happen the
    /// error is logged and the caller falls through to an unauthenticated
    /// request, which the API surfaces as `AccessDenied` (403) rather than a
    /// 500.
    pub fn authorization_header(&self, subject: &str) -> Option<String> {
        if subject == PROXY_SELF_SUBJECT {
            tracing::error!("refusing to sign an on-behalf-of assertion as the proxy itself");
            return None;
        }
        self.sign(subject)
    }

    /// The `Authorization` header value for a request the proxy makes as
    /// itself. See `PROXY_SELF_SUBJECT`.
    pub fn authorization_header_as_self(&self) -> Option<String> {
        self.sign(PROXY_SELF_SUBJECT)
    }

    /// The header for `caller`, or `None` when the request goes out anonymous.
    pub fn authorization_header_for(&self, caller: ApiCaller<'_>) -> Option<String> {
        match caller {
            ApiCaller::Anonymous => None,
            ApiCaller::Account(subject) => self.authorization_header(subject),
            ApiCaller::Proxy => self.authorization_header_as_self(),
        }
    }

    fn sign(&self, subject: &str) -> Option<String> {
        match self.signer.sign(subject, &self.issuer, &self.audience, &[]) {
            Ok(token) => Some(format!("Bearer {}", token)),
            Err(e) => {
                tracing::error!("failed to sign API auth JWT: {}", e);
                None
            }
        }
    }
}
