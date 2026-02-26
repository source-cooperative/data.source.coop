use crate::sts::Credentials;
use chrono::Utc;
use std::fs;
use std::io;
use std::path::PathBuf;

/// Replace any character that isn't alphanumeric, `-`, or `_` with `_`.
fn sanitize_role_arn(role_arn: &str) -> String {
    role_arn
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Full path to the credentials cache file for a given role.
fn cache_path(role_arn: &str) -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    let sanitized = sanitize_role_arn(role_arn);
    Ok(home
        .join(".source-coop")
        .join("credentials")
        .join(format!("{sanitized}.json")))
}

/// Write credentials to the per-role cache file.
/// Creates `~/.source-coop/credentials/` if it does not exist.
/// Sets file permissions to 0600 on Unix.
pub fn write_credentials(role_arn: &str, creds: &Credentials) -> Result<PathBuf, String> {
    let path = cache_path(role_arn)?;
    let dir = path.parent().unwrap();

    fs::create_dir_all(dir)
        .map_err(|e| format!("Failed to create cache directory {}: {e}", dir.display()))?;

    let json = serde_json::to_string_pretty(creds)
        .map_err(|e| format!("Failed to serialize credentials: {e}"))?;

    fs::write(&path, &json)
        .map_err(|e| format!("Failed to write credentials cache {}: {e}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("Failed to set permissions on {}: {e}", path.display()))?;
    }

    Ok(path)
}

/// Read credentials from the cache file for a given role.
/// Returns `None` if the file does not exist.
pub fn read_credentials(role_arn: &str) -> Result<Option<Credentials>, String> {
    let path = cache_path(role_arn)?;
    match fs::read_to_string(&path) {
        Ok(contents) => {
            let creds: Credentials = serde_json::from_str(&contents)
                .map_err(|e| format!("Failed to parse credentials cache: {e}"))?;
            Ok(Some(creds))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!(
            "Failed to read credentials cache {}: {e}",
            path.display()
        )),
    }
}

/// Check if credentials are expired or will expire within a 60-second buffer.
pub fn is_expired(creds: &Credentials) -> Result<bool, String> {
    let expiration = chrono::DateTime::parse_from_rfc3339(&creds.expiration).map_err(|e| {
        format!(
            "Failed to parse expiration timestamp '{}': {e}",
            creds.expiration
        )
    })?;

    let now = Utc::now();
    let buffer = chrono::Duration::seconds(60);

    Ok(expiration <= now + buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_creds(expiration: &str) -> Credentials {
        Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: "FwoGZXIvYXdzEtest".to_string(),
            expiration: expiration.to_string(),
        }
    }

    #[test]
    fn sanitize_simple_name() {
        assert_eq!(sanitize_role_arn("source-coop-user"), "source-coop-user");
    }

    #[test]
    fn sanitize_arn_with_special_chars() {
        assert_eq!(
            sanitize_role_arn("arn:aws:iam::123:role/Foo"),
            "arn_aws_iam__123_role_Foo"
        );
    }

    #[test]
    fn sanitize_preserves_underscores() {
        assert_eq!(sanitize_role_arn("my_role-name"), "my_role-name");
    }

    #[test]
    fn expired_future_date() {
        let future = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let creds = sample_creds(&future);
        assert!(!is_expired(&creds).unwrap());
    }

    #[test]
    fn expired_past_date() {
        let past = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let creds = sample_creds(&past);
        assert!(is_expired(&creds).unwrap());
    }

    #[test]
    fn expired_within_buffer() {
        // 30 seconds from now is within the 60s buffer
        let near_future = (Utc::now() + chrono::Duration::seconds(30)).to_rfc3339();
        let creds = sample_creds(&near_future);
        assert!(is_expired(&creds).unwrap());
    }

    #[test]
    fn expired_invalid_timestamp() {
        let creds = sample_creds("not-a-timestamp");
        assert!(is_expired(&creds).is_err());
    }

    #[test]
    fn round_trip_serialization() {
        let creds = sample_creds("2026-03-01T00:00:00Z");
        let json = serde_json::to_string_pretty(&creds).unwrap();
        let loaded: Credentials = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.access_key_id, creds.access_key_id);
        assert_eq!(loaded.secret_access_key, creds.secret_access_key);
        assert_eq!(loaded.session_token, creds.session_token);
        assert_eq!(loaded.expiration, creds.expiration);
    }
}
