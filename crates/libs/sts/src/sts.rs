//! STS credential minting.

use chrono::{Duration, Utc};
use s3_proxy_core::types::{AccessScope, RoleConfig, TemporaryCredentials};
use uuid::Uuid;

/// Resolve `{claim_name}` template variables in access scopes against JWT claims.
///
/// Each `{name}` in `bucket` or `prefixes` is replaced with the corresponding
/// string claim value. Missing or non-string claims resolve to an empty string,
/// which will safely fail authorization downstream.
fn resolve_scopes(scopes: &[AccessScope], claims: &serde_json::Value) -> Vec<AccessScope> {
    scopes
        .iter()
        .map(|scope| {
            let bucket = resolve_template(&scope.bucket, claims);
            let prefixes = scope
                .prefixes
                .iter()
                .map(|p| resolve_template(p, claims))
                .collect();
            AccessScope {
                bucket,
                prefixes,
                actions: scope.actions.clone(),
            }
        })
        .collect()
}

/// Replace all `{key}` placeholders in `template` with values from `claims`.
fn resolve_template(template: &str, claims: &serde_json::Value) -> String {
    let mut result = template.to_string();
    // Find all {…} placeholders and replace them
    while let Some(start) = result.find('{') {
        if let Some(end) = result[start..].find('}') {
            let end = start + end;
            let key = &result[start + 1..end];
            let value = claims
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("");
            result = format!("{}{}{}", &result[..start], value, &result[end + 1..]);
        } else {
            break;
        }
    }
    result
}

/// Mint a new set of temporary credentials for an assumed role.
///
/// Template variables (`{claim_name}`) in `role.allowed_scopes` are resolved
/// against the provided JWT `claims` before being stored in the credentials.
pub fn mint_temporary_credentials(
    role: &RoleConfig,
    source_identity: &str,
    duration_seconds: u64,
    key_prefix: &str,
    claims: &serde_json::Value,
) -> TemporaryCredentials {
    let access_key_id = format!("{}{}", key_prefix, generate_random_id(16));
    let secret_access_key = generate_random_id(40);
    let session_token = generate_session_token();

    let expiration = Utc::now() + Duration::seconds(duration_seconds as i64);

    TemporaryCredentials {
        access_key_id,
        secret_access_key,
        session_token,
        expiration,
        allowed_scopes: resolve_scopes(&role.allowed_scopes, claims),
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

#[cfg(test)]
mod tests {
    use super::*;
    use s3_proxy_core::types::Action;
    use serde_json::json;

    fn scope(bucket: &str, prefixes: &[&str], actions: &[Action]) -> AccessScope {
        AccessScope {
            bucket: bucket.to_string(),
            prefixes: prefixes.iter().map(|s| s.to_string()).collect(),
            actions: actions.to_vec(),
        }
    }

    #[test]
    fn resolve_template_in_bucket() {
        let scopes = vec![scope("{sub}", &[], &[Action::GetObject])];
        let claims = json!({"sub": "alice"});
        let resolved = resolve_scopes(&scopes, &claims);
        assert_eq!(resolved[0].bucket, "alice");
    }

    #[test]
    fn resolve_template_in_prefix() {
        let scopes = vec![scope("my-bucket", &["data/{sub}/"], &[Action::GetObject])];
        let claims = json!({"sub": "alice"});
        let resolved = resolve_scopes(&scopes, &claims);
        assert_eq!(resolved[0].prefixes[0], "data/alice/");
    }

    #[test]
    fn resolve_multiple_claims() {
        let scopes = vec![scope("{org}", &["{sub}/"], &[Action::GetObject])];
        let claims = json!({"sub": "alice", "org": "acme"});
        let resolved = resolve_scopes(&scopes, &claims);
        assert_eq!(resolved[0].bucket, "acme");
        assert_eq!(resolved[0].prefixes[0], "alice/");
    }

    #[test]
    fn no_templates_unchanged() {
        let scopes = vec![scope("static-bucket", &["prefix/"], &[Action::GetObject])];
        let claims = json!({"sub": "alice"});
        let resolved = resolve_scopes(&scopes, &claims);
        assert_eq!(resolved[0].bucket, "static-bucket");
        assert_eq!(resolved[0].prefixes[0], "prefix/");
    }

    #[test]
    fn missing_claim_resolves_to_empty() {
        let scopes = vec![scope("{missing}", &["{also_missing}/"], &[Action::GetObject])];
        let claims = json!({"sub": "alice"});
        let resolved = resolve_scopes(&scopes, &claims);
        assert_eq!(resolved[0].bucket, "");
        assert_eq!(resolved[0].prefixes[0], "/");
    }
}
