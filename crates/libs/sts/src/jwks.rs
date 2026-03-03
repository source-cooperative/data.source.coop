//! JWKS fetching and JWT verification.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};

use base64::Engine;
use rsa::pkcs1v15::VerifyingKey;
use rsa::signature::Verifier;
use rsa::{BigUint, RsaPublicKey};
use serde::Deserialize;
use sha2::Sha256;
use source_coop_core::error::ProxyError;
use source_coop_core::types::RoleConfig;

#[derive(Debug, Clone, Deserialize)]
pub struct JwksResponse {
    pub keys: Vec<JwkKey>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JwkKey {
    pub kid: String,
    pub kty: String,
    pub alg: Option<String>,
    pub n: Option<String>,
    pub e: Option<String>,
    #[serde(rename = "use")]
    pub use_: Option<String>,
}

/// Fetch JWKS from an OIDC provider's well-known endpoint.
///
/// Requires HTTPS issuer URLs per the OIDC specification. HTTP URLs are
/// rejected to prevent MITM attacks on JWKS discovery.
pub async fn fetch_jwks(
    client: &reqwest::Client,
    issuer: &str,
) -> Result<JwksResponse, ProxyError> {
    let issuer = issuer.trim_end_matches('/');

    if !issuer.starts_with("https://") {
        return Err(ProxyError::ConfigError(format!(
            "OIDC issuer must use HTTPS: {}",
            issuer
        )));
    }

    // First, try the .well-known/openid-configuration endpoint
    let config_url = format!("{}/.well-known/openid-configuration", issuer);

    let config_resp =
        client.get(&config_url).send().await.map_err(|e| {
            ProxyError::InvalidOidcToken(format!("failed to fetch OIDC config: {}", e))
        })?;

    let config: serde_json::Value = config_resp
        .json()
        .await
        .map_err(|e| ProxyError::InvalidOidcToken(format!("invalid OIDC config: {}", e)))?;

    let jwks_uri = config
        .get("jwks_uri")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProxyError::InvalidOidcToken("OIDC config missing jwks_uri".into()))?;

    // Fetch the JWKS
    let jwks_resp = client
        .get(jwks_uri)
        .send()
        .await
        .map_err(|e| ProxyError::InvalidOidcToken(format!("failed to fetch JWKS: {}", e)))?;

    jwks_resp
        .json()
        .await
        .map_err(|e| ProxyError::InvalidOidcToken(format!("invalid JWKS: {}", e)))
}

/// Find a key in the JWKS by key ID.
pub fn find_key<'a>(jwks: &'a JwksResponse, kid: &str) -> Result<&'a JwkKey, ProxyError> {
    jwks.keys
        .iter()
        .find(|k| k.kid == kid)
        .ok_or_else(|| ProxyError::InvalidOidcToken(format!("key '{}' not found in JWKS", kid)))
}

/// Decode a base64url-encoded string (no padding).
fn base64url_decode(input: &str) -> Result<Vec<u8>, ProxyError> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(input)
        .map_err(|e| ProxyError::InvalidOidcToken(format!("base64url decode error: {}", e)))
}

/// Build an RSA public key from JWK `n` and `e` components.
fn rsa_public_key_from_components(n: &str, e: &str) -> Result<RsaPublicKey, ProxyError> {
    let n_bytes = base64url_decode(n)?;
    let e_bytes = base64url_decode(e)?;
    let n_int = BigUint::from_bytes_be(&n_bytes);
    let e_int = BigUint::from_bytes_be(&e_bytes);
    RsaPublicKey::new(n_int, e_int)
        .map_err(|e| ProxyError::InvalidOidcToken(format!("invalid RSA key: {}", e)))
}

/// Verify a JWT using the provided JWK.
pub fn verify_token(
    token: &str,
    key: &JwkKey,
    issuer: &str,
    role: &RoleConfig,
) -> Result<serde_json::Value, ProxyError> {
    let n = key
        .n
        .as_ref()
        .ok_or_else(|| ProxyError::InvalidOidcToken("JWK missing 'n' component".into()))?;
    let e = key
        .e
        .as_ref()
        .ok_or_else(|| ProxyError::InvalidOidcToken("JWK missing 'e' component".into()))?;

    // Split the JWT into parts
    let parts: Vec<&str> = token.splitn(3, '.').collect();
    if parts.len() != 3 {
        return Err(ProxyError::InvalidOidcToken("malformed JWT".into()));
    }
    let [header_b64, payload_b64, signature_b64] = [parts[0], parts[1], parts[2]];

    // Verify the header specifies RS256
    let header_bytes = base64url_decode(header_b64)?;
    let header: serde_json::Value = serde_json::from_slice(&header_bytes)
        .map_err(|e| ProxyError::InvalidOidcToken(format!("invalid JWT header JSON: {}", e)))?;
    let alg = header.get("alg").and_then(|v| v.as_str()).unwrap_or("");
    if alg != "RS256" {
        return Err(ProxyError::InvalidOidcToken(format!(
            "unsupported JWT algorithm: {}",
            alg
        )));
    }

    // Verify the RSA signature
    let public_key = rsa_public_key_from_components(n, e)?;
    let verifying_key = VerifyingKey::<Sha256>::new(public_key);
    let signature_bytes = base64url_decode(signature_b64)?;
    let signature = rsa::pkcs1v15::Signature::try_from(signature_bytes.as_slice())
        .map_err(|e| ProxyError::InvalidOidcToken(format!("invalid signature: {}", e)))?;
    let signed_content = format!("{}.{}", header_b64, payload_b64);
    verifying_key
        .verify(signed_content.as_bytes(), &signature)
        .map_err(|e| {
            ProxyError::InvalidOidcToken(format!("JWT signature verification failed: {}", e))
        })?;

    // Decode and validate claims
    let payload_bytes = base64url_decode(payload_b64)?;
    let claims: serde_json::Value = serde_json::from_slice(&payload_bytes)
        .map_err(|e| ProxyError::InvalidOidcToken(format!("invalid JWT payload JSON: {}", e)))?;

    // Validate issuer
    let token_issuer = claims.get("iss").and_then(|v| v.as_str()).unwrap_or("");
    if token_issuer != issuer {
        return Err(ProxyError::InvalidOidcToken(format!(
            "issuer mismatch: expected {}, got {}",
            issuer, token_issuer
        )));
    }

    // Validate audience if required
    if let Some(ref required_aud) = role.required_audience {
        let aud_valid = match claims.get("aud") {
            Some(serde_json::Value::String(aud)) => aud == required_aud,
            Some(serde_json::Value::Array(auds)) => auds
                .iter()
                .any(|a| a.as_str() == Some(required_aud.as_str())),
            _ => false,
        };
        if !aud_valid {
            return Err(ProxyError::InvalidOidcToken(format!(
                "audience mismatch: expected {}",
                required_aud
            )));
        }
    }

    // Validate time-based claims with clock skew tolerance
    let now = chrono::Utc::now().timestamp();
    const CLOCK_SKEW_SECS: i64 = 60;

    if let Some(exp) = claims.get("exp").and_then(|v| v.as_i64()) {
        if now > exp + CLOCK_SKEW_SECS {
            return Err(ProxyError::InvalidOidcToken("token has expired".into()));
        }
    }

    if let Some(nbf) = claims.get("nbf").and_then(|v| v.as_i64()) {
        if now < nbf - CLOCK_SKEW_SECS {
            return Err(ProxyError::InvalidOidcToken(
                "token is not yet valid".into(),
            ));
        }
    }

    Ok(claims)
}

/// In-memory cache for JWKS responses, keyed by issuer URL.
///
/// OIDC providers publish JWKS keys that change infrequently. Caching avoids
/// a network round-trip to the provider on every token validation and prevents
/// DoS via repeated validation attempts.
///
/// Failed fetches are cached with a shorter TTL (`failure_ttl`) to avoid
/// hammering broken endpoints while still retrying periodically.
///
/// Uses `DateTime<Utc>` instead of `std::time::Instant` for WASM compatibility
/// (`Instant` panics on `wasm32-unknown-unknown`).
pub struct JwksCache {
    client: reqwest::Client,
    ttl: Duration,
    failure_ttl: Duration,
    entries: Mutex<HashMap<String, (DateTime<Utc>, JwksResponse)>>,
    failures: Mutex<HashMap<String, DateTime<Utc>>>,
}

impl JwksCache {
    /// Create a new cache with the given TTL and HTTP client.
    pub fn new(client: reqwest::Client, ttl: Duration) -> Self {
        Self {
            client,
            ttl,
            failure_ttl: Duration::from_secs(30),
            entries: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
        }
    }

    /// Fetch JWKS for the given issuer, returning a cached response if fresh.
    pub async fn get_or_fetch(&self, issuer: &str) -> Result<JwksResponse, ProxyError> {
        // Check cache
        if let Some(cached) = self.get_cached(issuer) {
            return Ok(cached);
        }

        // Check if we recently failed for this issuer
        {
            let failures = self.failures.lock().unwrap();
            if let Some(failed_at) = failures.get(issuer) {
                let elapsed = Utc::now().signed_duration_since(*failed_at).num_seconds();
                if elapsed >= 0 && (elapsed as u64) < self.failure_ttl.as_secs() {
                    return Err(ProxyError::InvalidOidcToken(format!(
                        "JWKS fetch for '{}' recently failed, retrying after backoff",
                        issuer
                    )));
                }
            }
        }

        // Cache miss — fetch from the network
        match fetch_jwks(&self.client, issuer).await {
            Ok(jwks) => {
                let mut entries = self.entries.lock().unwrap();
                entries.insert(issuer.to_string(), (Utc::now(), jwks.clone()));
                // Clear any failure state on success
                drop(entries);
                self.failures.lock().unwrap().remove(issuer);
                Ok(jwks)
            }
            Err(e) => {
                tracing::warn!(issuer = %issuer, error = %e, "JWKS fetch failed, backing off");
                self.failures
                    .lock()
                    .unwrap()
                    .insert(issuer.to_string(), Utc::now());
                Err(e)
            }
        }
    }

    fn get_cached(&self, issuer: &str) -> Option<JwksResponse> {
        let entries = self.entries.lock().unwrap();
        if let Some((fetched_at, jwks)) = entries.get(issuer) {
            let elapsed = Utc::now().signed_duration_since(*fetched_at).num_seconds();
            if elapsed >= 0 && (elapsed as u64) < self.ttl.as_secs() {
                return Some(jwks.clone());
            }
        }
        None
    }
}
