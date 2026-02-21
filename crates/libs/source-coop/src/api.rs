//! HTTP client for the Source Cooperative API.
//!
//! Makes server-to-server calls to resolve products, data connections,
//! API keys, and permissions. The actual HTTP transport is abstracted behind
//! the [`HttpClient`] trait so each runtime can provide its own implementation.

use s3_proxy_core::error::ProxyError;
use s3_proxy_core::maybe_send::{MaybeSend, MaybeSync};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::HashMap;
use std::future::Future;

/// Options for response caching.
pub struct CacheOptions {
    pub cache_ttl: u32,
    pub cache_key: Option<String>,
}

/// Trait abstracting HTTP JSON fetching so each runtime can provide its own implementation.
pub trait HttpClient: Clone + MaybeSend + MaybeSync + 'static {
    fn fetch_json<T: DeserializeOwned + MaybeSend>(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        cache: Option<&CacheOptions>,
    ) -> impl Future<Output = Result<T, ProxyError>> + MaybeSend;
}

/// Per-endpoint cache TTLs (seconds). Set to 0 to disable caching.
pub struct CacheTtls {
    pub product: u32,
    pub data_connection: u32,
    pub permissions: u32,
    pub account: u32,
    pub api_key: u32,
}

impl Default for CacheTtls {
    fn default() -> Self {
        Self {
            product: 5 * 60,
            data_connection: 30 * 60,
            permissions: 60,
            account: 5 * 60,
            api_key: 60,
        }
    }
}

/// Client for the Source Cooperative API.
#[derive(Clone)]
pub struct SourceApiClient<H> {
    http: H,
    api_url: String,
    api_key: String,
    product_cache_ttl: u32,
    data_connection_cache_ttl: u32,
    permissions_cache_ttl: u32,
    account_cache_ttl: u32,
    api_key_cache_ttl: u32,
}

// -- API response types --

#[derive(Debug, Deserialize)]
pub struct SourceProduct {
    pub disabled: bool,
    pub data_mode: String,
    pub metadata: ProductMetadata,
}

#[derive(Debug, Deserialize)]
pub struct ProductMetadata {
    pub primary_mirror: String,
    pub mirrors: HashMap<String, ProductMirror>,
}

#[derive(Debug, Deserialize)]
pub struct ProductMirror {
    pub connection_id: String,
    pub prefix: String,
}

#[derive(Debug, Deserialize)]
pub struct DataConnection {
    pub details: ConnectionDetails,
    pub authentication: ConnectionAuth,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ConnectionDetails {
    pub provider: String,
    pub region: Option<String>,
    pub base_prefix: Option<String>,
    pub bucket: Option<String>,
    #[serde(default)]
    pub account_name: Option<String>,
    #[serde(default)]
    pub container_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ConnectionAuth {
    pub auth_type: String,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    #[serde(default)]
    pub access_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SourceApiKey {
    pub access_key_id: String,
    pub secret_access_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum RepositoryPermission {
    Read,
    Write,
}

/// API response for the permissions endpoint.
#[derive(Debug, Deserialize)]
pub struct PermissionsResponse {
    #[serde(default)]
    pub read: bool,
    #[serde(default)]
    pub write: bool,
}

/// API response for account listing — contains repositories.
#[derive(Debug, Deserialize)]
pub struct AccountResponse {
    #[serde(default)]
    pub repositories: Vec<AccountRepository>,
}

#[derive(Debug, Deserialize)]
pub struct AccountRepository {
    pub repository_id: String,
}

// -- Client implementation --

impl<H: HttpClient> SourceApiClient<H> {
    pub fn new(http: H, api_url: String, api_key: String, cache_ttls: CacheTtls) -> Self {
        Self {
            http,
            api_url,
            api_key,
            product_cache_ttl: cache_ttls.product,
            data_connection_cache_ttl: cache_ttls.data_connection,
            permissions_cache_ttl: cache_ttls.permissions,
            account_cache_ttl: cache_ttls.account,
            api_key_cache_ttl: cache_ttls.api_key,
        }
    }

    fn auth_headers(&self) -> Vec<(&str, String)> {
        vec![("authorization", self.api_key.clone())]
    }

    /// `GET /api/v1/products/{account_id}/{repo_id}`
    pub async fn get_product(
        &self,
        account_id: &str,
        repo_id: &str,
    ) -> Result<SourceProduct, ProxyError> {
        let url = format!(
            "{}/api/v1/products/{}/{}",
            self.api_url, account_id, repo_id
        );
        let auth = self.auth_headers();
        let headers: Vec<(&str, &str)> = auth.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let cache = (self.product_cache_ttl > 0).then(|| CacheOptions {
            cache_ttl: self.product_cache_ttl,
            cache_key: None,
        });
        self.http.fetch_json(&url, &headers, cache.as_ref()).await
    }

    /// `GET /api/v1/data-connections/{id}`
    pub async fn get_data_connection(&self, id: &str) -> Result<DataConnection, ProxyError> {
        let url = format!("{}/api/v1/data-connections/{}", self.api_url, id);
        let auth = self.auth_headers();
        let headers: Vec<(&str, &str)> = auth.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let cache = (self.data_connection_cache_ttl > 0).then(|| CacheOptions {
            cache_ttl: self.data_connection_cache_ttl,
            cache_key: None,
        });
        self.http.fetch_json(&url, &headers, cache.as_ref()).await
    }

    /// `GET /api/v1/api-keys/{access_key_id}/auth`
    pub async fn get_api_key(&self, access_key_id: &str) -> Result<SourceApiKey, ProxyError> {
        let url = format!("{}/api/v1/api-keys/{}/auth", self.api_url, access_key_id);
        let auth = self.auth_headers();
        let headers: Vec<(&str, &str)> = auth.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let cache = (self.api_key_cache_ttl > 0).then(|| CacheOptions {
            cache_ttl: self.api_key_cache_ttl,
            cache_key: None,
        });
        self.http.fetch_json(&url, &headers, cache.as_ref()).await
    }

    /// `GET /api/v1/products/{account_id}/{repo_id}/permissions`
    ///
    /// Uses the *user's* API key (not the server key) to check permissions
    /// for the authenticated user.
    ///
    /// A custom `cacheKey` incorporating the user's API key prevents
    /// cross-user cache poisoning (the URL is the same for all users).
    pub async fn get_permissions(
        &self,
        account_id: &str,
        repo_id: &str,
        user_api_key: &str,
    ) -> Result<PermissionsResponse, ProxyError> {
        let url = format!(
            "{}/api/v1/products/{}/{}/permissions",
            self.api_url, account_id, repo_id
        );
        let headers = [("authorization", user_api_key)];
        let cache = (self.permissions_cache_ttl > 0).then(|| CacheOptions {
            cache_ttl: self.permissions_cache_ttl,
            cache_key: Some(format!(
                "source-perms:{}:{}:{}",
                account_id, repo_id, user_api_key
            )),
        });
        self.http.fetch_json(&url, &headers, cache.as_ref()).await
    }

    /// `GET /api/v1/accounts/{account_id}`
    pub async fn list_account_repos(
        &self,
        account_id: &str,
    ) -> Result<AccountResponse, ProxyError> {
        let url = format!("{}/api/v1/accounts/{}", self.api_url, account_id);
        let auth = self.auth_headers();
        let headers: Vec<(&str, &str)> = auth.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let cache = (self.account_cache_ttl > 0).then(|| CacheOptions {
            cache_ttl: self.account_cache_ttl,
            cache_key: None,
        });
        self.http.fetch_json(&url, &headers, cache.as_ref()).await
    }
}
