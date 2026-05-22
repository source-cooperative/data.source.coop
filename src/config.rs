use std::sync::OnceLock;

use multistore_oidc_provider::jwt::JwtSigner;
use multistore_sts::sealed_token::TokenKey;
use worker::Env;

static CONFIG: OnceLock<AppConfig> = OnceLock::new();

/// Return the process-wide config, parsing env/secrets the first time it's
/// called. Subsequent calls within the same isolate are free — in particular,
/// the RSA OIDC signing keys are parsed from PEM exactly once.
pub fn load_config(env: &Env) -> &'static AppConfig {
    CONFIG.get_or_init(|| build_config(env))
}

fn build_config(env: &Env) -> AppConfig {
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
        let previous_signer = env
            .secret("OIDC_PROVIDER_KEY_PREVIOUS")
            .ok()
            .and_then(|prev_pem| {
                let prev_kid = env.var("OIDC_PROVIDER_KID_PREVIOUS").ok()?.to_string();
                match JwtSigner::from_pem(&prev_pem.to_string(), prev_kid, 60) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        tracing::warn!("failed to load previous OIDC key: {}", e);
                        None
                    }
                }
            });

        OidcConfig {
            signer,
            issuer,
            previous_signer,
        }
    };

    let session_token_key = TokenKey::from_base64(
        &env.secret("SESSION_TOKEN_KEY")
            .expect("SESSION_TOKEN_KEY must be set")
            .to_string(),
    )
    .expect("SESSION_TOKEN_KEY must be valid base64-encoded 32-byte key");

    let auth_issuer = env
        .var("AUTH_ISSUER")
        .map(|v| v.to_string())
        .expect("AUTH_ISSUER must be set");

    AppConfig {
        api_base_url,
        oidc,
        session_token_key,
        auth_issuer,
    }
}

pub struct AppConfig {
    pub api_base_url: String,
    pub oidc: OidcConfig,
    /// AES key for sealing/unsealing STS session tokens.
    pub session_token_key: TokenKey,
    /// OIDC issuer URL for the Source Cooperative auth provider (e.g. `https://auth.source.coop`).
    pub auth_issuer: String,
}

pub struct OidcConfig {
    pub issuer: String,
    pub signer: JwtSigner,
    /// Previous key for rotation — served in JWKS but not used for signing.
    pub previous_signer: Option<JwtSigner>,
}
