//! STS credential minting.

use chrono::{Duration, Utc};
use s3_proxy_core::types::{RoleConfig, TemporaryCredentials};
use uuid::Uuid;

/// Mint a new set of temporary credentials for an assumed role.
pub fn mint_temporary_credentials(
    role: &RoleConfig,
    source_identity: &str,
    duration_seconds: u64,
) -> TemporaryCredentials {
    let access_key_id = format!("ASIA{}", generate_random_id(16));
    let secret_access_key = generate_random_id(40);
    let session_token = generate_session_token();

    let expiration = Utc::now() + Duration::seconds(duration_seconds as i64);

    TemporaryCredentials {
        access_key_id,
        secret_access_key,
        session_token,
        expiration,
        allowed_scopes: role.allowed_scopes.clone(),
        assumed_role_id: role.role_id.clone(),
        source_identity: source_identity.to_string(),
    }
}

fn generate_random_id(len: usize) -> String {
    use base64::Engine;
    let bytes: Vec<u8> = (0..len).map(|_| rand_byte()).collect();
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes);
    // Take only alphanumeric chars to match AWS key format
    encoded
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(len)
        .collect()
}

fn generate_session_token() -> String {
    // Real AWS session tokens are much longer; this is a simplified version
    let id = Uuid::new_v4();
    format!("FwoGZXIvYXdzE{}", id.to_string().replace('-', ""))
}

/// Simple random byte using UUID as entropy source (avoids extra deps).
fn rand_byte() -> u8 {
    let id = Uuid::new_v4();
    id.as_bytes()[0]
}
