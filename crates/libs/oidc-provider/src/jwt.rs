//! JWT minting — sign JWTs with the proxy's RSA private key.

use base64::Engine;
use chrono::{Duration, Utc};
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::RsaPrivateKey;
use sha2::Sha256;
use uuid::Uuid;

use crate::OidcProviderError;

/// Signs JWTs using an RSA private key (RS256).
#[derive(Clone)]
pub struct JwtSigner {
    private_key: RsaPrivateKey,
    kid: String,
    ttl_seconds: i64,
}

impl JwtSigner {
    /// Create a signer from a PEM-encoded PKCS#8 private key.
    pub fn from_pem(pem: &str, kid: String, ttl_seconds: i64) -> Result<Self, OidcProviderError> {
        let private_key = RsaPrivateKey::from_pkcs8_pem(pem).map_err(|e| {
            OidcProviderError::KeyError(format!("failed to parse private key: {e}"))
        })?;
        Ok(Self {
            private_key,
            kid,
            ttl_seconds,
        })
    }

    /// The key ID used in JWT headers and JWKS.
    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// Access the public key for JWKS generation.
    pub fn public_key(&self) -> &rsa::RsaPublicKey {
        self.private_key.as_ref()
    }

    /// Sign a JWT with the given claims.
    pub fn sign(
        &self,
        subject: &str,
        issuer: &str,
        audience: &str,
        extra_claims: &[(&str, &str)],
    ) -> Result<String, OidcProviderError> {
        let now = Utc::now();
        let exp = now + Duration::seconds(self.ttl_seconds);
        let jti = Uuid::new_v4().to_string();

        let b64 = &base64::engine::general_purpose::URL_SAFE_NO_PAD;

        // Header
        let header = serde_json::json!({
            "alg": "RS256",
            "typ": "JWT",
            "kid": self.kid,
        });
        let header_b64 = b64.encode(header.to_string().as_bytes());

        // Payload
        let mut payload = serde_json::json!({
            "iss": issuer,
            "sub": subject,
            "aud": audience,
            "exp": exp.timestamp(),
            "iat": now.timestamp(),
            "nbf": now.timestamp(),
            "jti": jti,
        });
        if let serde_json::Value::Object(ref mut map) = payload {
            for (k, v) in extra_claims {
                map.insert(
                    (*k).to_string(),
                    serde_json::Value::String((*v).to_string()),
                );
            }
        }
        let payload_b64 = b64.encode(payload.to_string().as_bytes());

        // Sign
        let signing_input = format!("{header_b64}.{payload_b64}");
        let signing_key = SigningKey::<Sha256>::new(self.private_key.clone());
        let signature = signing_key.sign(signing_input.as_bytes());
        let sig_b64 = b64.encode(signature.to_bytes());

        Ok(format!("{signing_input}.{sig_b64}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key_pem() -> String {
        // Generate a small RSA key for testing
        use rsa::pkcs8::EncodePrivateKey;
        let mut rng = rand::rngs::OsRng;
        let key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap()
            .to_string()
    }

    #[test]
    fn sign_produces_three_part_jwt() {
        let pem = test_key_pem();
        let signer = JwtSigner::from_pem(&pem, "test-kid".into(), 300).unwrap();
        let token = signer
            .sign(
                "my-subject",
                "https://proxy.example.com",
                "sts.amazonaws.com",
                &[],
            )
            .unwrap();

        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT should have header.payload.signature");

        // Decode header and check kid
        let header_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[0])
            .unwrap();
        let header: serde_json::Value = serde_json::from_slice(&header_bytes).unwrap();
        assert_eq!(header["alg"], "RS256");
        assert_eq!(header["kid"], "test-kid");

        // Decode payload and check standard claims
        let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[1])
            .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).unwrap();
        assert_eq!(payload["iss"], "https://proxy.example.com");
        assert_eq!(payload["sub"], "my-subject");
        assert_eq!(payload["aud"], "sts.amazonaws.com");
        assert!(payload["exp"].as_i64().unwrap() > payload["iat"].as_i64().unwrap());
    }

    #[test]
    fn sign_includes_extra_claims() {
        let pem = test_key_pem();
        let signer = JwtSigner::from_pem(&pem, "k1".into(), 60).unwrap();
        let token = signer
            .sign("sub", "iss", "aud", &[("custom_key", "custom_value")])
            .unwrap();

        let parts: Vec<&str> = token.split('.').collect();
        let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[1])
            .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).unwrap();
        assert_eq!(payload["custom_key"], "custom_value");
    }

    #[test]
    fn signature_is_verifiable() {
        use rsa::pkcs1v15::VerifyingKey;
        use rsa::signature::Verifier;

        let pem = test_key_pem();
        let signer = JwtSigner::from_pem(&pem, "k1".into(), 300).unwrap();
        let token = signer.sign("s", "i", "a", &[]).unwrap();

        let parts: Vec<&str> = token.split('.').collect();
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let sig_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[2])
            .unwrap();
        let signature = rsa::pkcs1v15::Signature::try_from(sig_bytes.as_slice()).unwrap();

        let verifying_key = VerifyingKey::<Sha256>::new(signer.public_key().clone());
        verifying_key
            .verify(signing_input.as_bytes(), &signature)
            .expect("signature should verify");
    }
}
