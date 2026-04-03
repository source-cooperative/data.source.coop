use multistore_oidc_provider::jwt::JwtSigner;
use worker::Env;

pub fn load_config(env: &Env) -> AppConfig {
    let api_base_url = env
        .var("SOURCE_API_URL")
        .map(|v| v.to_string())
        .expect("SOURCE_API_URL must be set");
    let oidc = {
        let pem = env
            .secret("OIDC_PROVIDER_KEY")
            .expect("OIDC_PROVIDER_KEY must be set")
            .to_string();
        let kid = env
            .var("OIDC_PROVIDER_KID")
            .expect("OIDC_PROVIDER_KID must be set")
            .to_string();
        let issuer = env
            .var("OIDC_PROVIDER_ISSUER")
            .expect("OIDC_PROVIDER_ISSUER must be set")
            .to_string();

        let signer = JwtSigner::from_pem(&pem, kid, 60)
            .expect("failed to create JwtSigner from OIDC_PROVIDER_KEY");

        // Optional previous key for rotation
        let previous_signer = {
            let prev_pem = env
                .secret("OIDC_PROVIDER_KEY_PREVIOUS")
                .expect("OIDC_PROVIDER_KEY_PREVIOUS must be set")
                .to_string();
            let prev_kid = env
                .var("OIDC_PROVIDER_KID_PREVIOUS")
                .expect("OIDC_PROVIDER_KID_PREVIOUS must be set")
                .to_string();
            match JwtSigner::from_pem(&prev_pem, prev_kid, 60) {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!("failed to load previous OIDC key: {}", e);
                    None
                }
            }
        };

        OidcConfig {
            signer,
            issuer,
            previous_signer,
        }
    };

    AppConfig { api_base_url, oidc }
}

pub struct AppConfig {
    pub api_base_url: String,
    pub oidc: OidcConfig,
}

pub struct OidcConfig {
    pub issuer: String,
    pub signer: JwtSigner,
    /// Previous key for rotation — served in JWKS but not used for signing.
    pub previous_signer: Option<JwtSigner>,
}
