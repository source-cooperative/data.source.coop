//! Self-contained encrypted session tokens using AES-256-GCM.
//!
//! When a `TokenKey` is configured, temporary credentials are encrypted into
//! the session token itself. The proxy decrypts the token on each request —
//! no server-side storage lookup is needed. This is critical for stateless
//! runtimes like Cloudflare Workers where in-memory state does not persist
//! across invocations.

use crate::error::ProxyError;
use crate::types::TemporaryCredentials;
use aes_gcm::aead::{Aead, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, KeyInit};
use base64::Engine;
use std::sync::Arc;

const NONCE_LEN: usize = 12;

/// Wraps an AES-256-GCM cipher for sealing/unsealing session tokens.
#[derive(Clone)]
pub struct TokenKey(Arc<Aes256Gcm>);

impl TokenKey {
    /// Create a `TokenKey` from a base64-encoded 32-byte key.
    pub fn from_base64(encoded: &str) -> Result<Self, ProxyError> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|e| {
                ProxyError::ConfigError(format!("invalid SESSION_TOKEN_KEY base64: {e}"))
            })?;
        if bytes.len() != 32 {
            return Err(ProxyError::ConfigError(format!(
                "SESSION_TOKEN_KEY must be 32 bytes, got {}",
                bytes.len()
            )));
        }
        let cipher = Aes256Gcm::new_from_slice(&bytes)
            .map_err(|e| ProxyError::ConfigError(format!("AES key error: {e}")))?;
        Ok(Self(Arc::new(cipher)))
    }

    /// Encrypt `TemporaryCredentials` into a base64url token.
    ///
    /// Format: `base64url(nonce[12] || ciphertext+tag)`
    pub fn seal(&self, creds: &TemporaryCredentials) -> Result<String, ProxyError> {
        let plaintext = serde_json::to_vec(creds)
            .map_err(|e| ProxyError::Internal(format!("seal json: {e}")))?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = self
            .0
            .encrypt(&nonce, plaintext.as_slice())
            .map_err(|e| ProxyError::Internal(format!("seal encrypt: {e}")))?;

        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ciphertext);

        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&blob))
    }

    /// Decrypt a session token back into `TemporaryCredentials`.
    ///
    /// Returns `Ok(None)` if the token doesn't look like a sealed token
    /// (e.g. base64 decode fails or decryption fails — allows fallback to
    /// config-based lookup). Returns `Err(ExpiredCredentials)` when the
    /// token decrypts successfully but the credentials have expired.
    pub fn unseal(&self, token: &str) -> Result<Option<TemporaryCredentials>, ProxyError> {
        let blob = match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(token) {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };

        if blob.len() <= NONCE_LEN {
            return Ok(None);
        }

        let nonce = aes_gcm::Nonce::from_slice(&blob[..NONCE_LEN]);
        let ciphertext = &blob[NONCE_LEN..];

        let plaintext = match self.0.decrypt(nonce, ciphertext) {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };

        let creds: TemporaryCredentials = serde_json::from_slice(&plaintext)
            .map_err(|e| ProxyError::Internal(format!("unseal json: {e}")))?;

        if creds.expiration <= chrono::Utc::now() {
            return Err(ProxyError::ExpiredCredentials);
        }

        Ok(Some(creds))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AccessScope;

    fn make_key() -> TokenKey {
        let key_bytes = [0x42u8; 32];
        let encoded = base64::engine::general_purpose::STANDARD.encode(key_bytes);
        TokenKey::from_base64(&encoded).unwrap()
    }

    fn make_creds() -> TemporaryCredentials {
        TemporaryCredentials {
            access_key_id: "ASIATEMP".into(),
            secret_access_key: "secret".into(),
            session_token: "original-token".into(),
            expiration: chrono::Utc::now() + chrono::Duration::hours(1),
            allowed_scopes: vec![AccessScope {
                bucket: "test-bucket".into(),
                prefixes: vec![],
                actions: vec![crate::types::Action::GetObject],
            }],
            assumed_role_id: "role-1".into(),
            source_identity: "test".into(),
        }
    }

    #[test]
    fn round_trip() {
        let key = make_key();
        let creds = make_creds();
        let sealed = key.seal(&creds).unwrap();
        let unsealed = key.unseal(&sealed).unwrap().unwrap();
        assert_eq!(unsealed.access_key_id, creds.access_key_id);
        assert_eq!(unsealed.secret_access_key, creds.secret_access_key);
        assert_eq!(unsealed.assumed_role_id, creds.assumed_role_id);
    }

    #[test]
    fn wrong_key_returns_none() {
        let key1 = make_key();
        let key2 = {
            let key_bytes = [0x99u8; 32];
            let encoded = base64::engine::general_purpose::STANDARD.encode(key_bytes);
            TokenKey::from_base64(&encoded).unwrap()
        };
        let creds = make_creds();
        let sealed = key1.seal(&creds).unwrap();
        assert!(key2.unseal(&sealed).unwrap().is_none());
    }

    #[test]
    fn non_sealed_token_returns_none() {
        let key = make_key();
        assert!(key
            .unseal("FwoGZXIvYXdzEBYaDGFiY2RlZjEyMzQ1Ng")
            .unwrap()
            .is_none());
    }

    #[test]
    fn expired_token_returns_error() {
        let key = make_key();
        let mut creds = make_creds();
        creds.expiration = chrono::Utc::now() - chrono::Duration::hours(1);
        let sealed = key.seal(&creds).unwrap();
        let err = key.unseal(&sealed).unwrap_err();
        assert!(matches!(err, ProxyError::ExpiredCredentials));
    }

    #[test]
    fn invalid_key_length_rejected() {
        let short = base64::engine::general_purpose::STANDARD.encode([0u8; 16]);
        assert!(TokenKey::from_base64(&short).is_err());
    }
}
