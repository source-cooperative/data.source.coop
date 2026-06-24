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
                let prev_kid = match env.var("OIDC_PROVIDER_KID_PREVIOUS") {
                    Ok(k) => k.to_string(),
                    Err(_) => {
                        tracing::warn!(
                            "OIDC_PROVIDER_KEY_PREVIOUS is set but OIDC_PROVIDER_KID_PREVIOUS \
                             is missing -- previous key omitted from JWKS; set \
                             OIDC_PROVIDER_KID_PREVIOUS to complete rotation"
                        );
                        return None;
                    }
                };
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

    let auth_audience = env
        .var("AUTH_AUDIENCE")
        .map(|v| v.to_string())
        .ok()
        .filter(|s| !s.is_empty());
    if auth_audience.is_none() {
        // Fail closed: without an audience restriction, an ID token minted for
        // ANY OAuth client of AUTH_ISSUER could be exchanged for a user's
        // credentials, so /.sts is disabled entirely (returns 501) until set.
        tracing::warn!("AUTH_AUDIENCE not set: /.sts token exchange is disabled (returns 501)");
    }

    // Salt for hashing client IPs in analytics. Optional: when unset we still
    // hash (so raw IPs never land in the dataset), but without a secret salt the
    // small IPv4 space is brute-forceable, so prod should set it.
    let ip_hash_salt = env
        .secret("IP_HASH_SALT")
        .map(|v| v.to_string())
        .unwrap_or_default();
    if ip_hash_salt.is_empty() {
        tracing::warn!("IP_HASH_SALT not set: client-IP hashes are unsalted (brute-forceable)");
    }

    AppConfig {
        api_base_url,
        oidc,
        session_token_key,
        auth_issuer,
        auth_audience,
        ip_hash_salt,
    }
}

pub struct AppConfig {
    pub api_base_url: String,
    pub oidc: OidcConfig,
    /// AES key for sealing/unsealing STS session tokens.
    pub session_token_key: TokenKey,
    /// OIDC issuer URL for the Source Cooperative auth provider (e.g. `https://auth.source.coop`).
    pub auth_issuer: String,
    /// OAuth client ID that subject tokens presented to `/.sts` must be
    /// issued to (the `aud` claim). Required; `None` disables the `/.sts`
    /// endpoint entirely (returns 501) rather than accepting any audience.
    pub auth_audience: Option<String>,
    /// Secret salt for hashing client IPs before they enter analytics. Empty
    /// when `IP_HASH_SALT` is unset (hashes still happen, just unsalted).
    pub ip_hash_salt: String,
}

pub struct OidcConfig {
    pub issuer: String,
    pub signer: JwtSigner,
    /// Previous key for rotation — served in JWKS but not used for signing.
    pub previous_signer: Option<JwtSigner>,
}
