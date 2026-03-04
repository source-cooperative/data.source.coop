//! TTL credential cache.
//!
//! Caches [`CloudCredentials`] by key, evicting entries that are within a
//! safety margin of expiration. This avoids redundant STS calls when the
//! same backend is accessed repeatedly within a short window.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{Duration, Utc};

use crate::CloudCredentials;

/// Safety margin before expiration — credentials are considered expired
/// this many seconds before their actual `expires_at`.
const EXPIRY_MARGIN_SECS: i64 = 60;

/// Thread-safe TTL cache for cloud credentials.
pub struct CredentialCache {
    entries: Mutex<HashMap<String, Arc<CloudCredentials>>>,
}

impl Default for CredentialCache {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Retrieve cached credentials if they are still valid.
    pub fn get(&self, key: &str) -> Option<Arc<CloudCredentials>> {
        let entries = self.entries.lock().unwrap();
        if let Some(creds) = entries.get(key) {
            let margin = Duration::seconds(EXPIRY_MARGIN_SECS);
            if creds.expires_at > Utc::now() + margin {
                return Some(creds.clone());
            }
        }
        None
    }

    /// Store credentials in the cache.
    pub fn put(&self, key: String, creds: Arc<CloudCredentials>) {
        let mut entries = self.entries.lock().unwrap();
        entries.insert(key, creds);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_creds(expires_in_secs: i64) -> CloudCredentials {
        CloudCredentials {
            access_key_id: "AKID".into(),
            secret_access_key: "secret".into(),
            session_token: "token".into(),
            expires_at: Utc::now() + Duration::seconds(expires_in_secs),
        }
    }

    #[test]
    fn cache_returns_valid_entry() {
        let cache = CredentialCache::new();
        let creds = Arc::new(make_creds(600));
        cache.put("role-a".into(), creds.clone());

        let got = cache.get("role-a");
        assert!(got.is_some());
        assert_eq!(got.unwrap().access_key_id, "AKID");
    }

    #[test]
    fn cache_evicts_expired_entry() {
        let cache = CredentialCache::new();
        // Expires in 30 seconds — within the 60-second margin
        let creds = Arc::new(make_creds(30));
        cache.put("role-b".into(), creds);

        let got = cache.get("role-b");
        assert!(got.is_none());
    }

    #[test]
    fn cache_miss_for_unknown_key() {
        let cache = CredentialCache::new();
        assert!(cache.get("unknown").is_none());
    }
}
