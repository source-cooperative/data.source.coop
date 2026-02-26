//! Configuration provider abstraction and implementations.
//!
//! The [`ConfigProvider`] trait defines how the proxy retrieves its
//! configuration (buckets, roles, credentials) from a backend store.
//! This allows the same core logic to work with static files, databases,
//! HTTP APIs, or any other configuration source.
//!
//! # Available Implementations
//!
//! | Provider | Feature Flag | Use Case |
//! |----------|-------------|----------|
//! | [`StaticProvider`](static_file::StaticProvider) | *(always available)* | TOML/JSON config files, baked-in config |
//! | [`HttpProvider`](http::HttpProvider) | `config-http` | Centralized config API |
//! | [`DynamoDbProvider`](dynamodb::DynamoDbProvider) | `config-dynamodb` | AWS-native deployments |
//! | [`PostgresProvider`](postgres::PostgresProvider) | `config-postgres` | Database-backed config |
//!
//! # Caching
//!
//! Wrap any provider with [`CachedProvider`](cached::CachedProvider) to add
//! in-memory TTL-based caching. This is recommended for providers that make
//! network calls (HTTP, DynamoDB, Postgres).
//!
//! ```rust,ignore
//! use source_coop_core::config::{cached::CachedProvider, static_file::StaticProvider};
//! use std::time::Duration;
//!
//! let base = StaticProvider::from_file("config.toml").unwrap();
//! let cached = CachedProvider::new(base, Duration::from_secs(300));
//! ```

pub mod cached;
pub mod static_file;

#[cfg(feature = "config-http")]
pub mod http;

#[cfg(feature = "config-dynamodb")]
pub mod dynamodb;

#[cfg(feature = "config-postgres")]
pub mod postgres;

use crate::error::ProxyError;
use crate::maybe_send::{MaybeSend, MaybeSync};
use crate::types::{BucketConfig, RoleConfig, StoredCredential, TemporaryCredentials};
use std::future::Future;

/// Trait for retrieving proxy configuration from a backend store.
///
/// Implementations should be cheap to clone (wrap inner state in `Arc`).
///
/// Methods use [`MaybeSend`] bounds — on native targets this resolves to `Send`
/// (required by Tokio's task spawning), on WASM it's a no-op (allowing `!Send`
/// JS interop types).
pub trait ConfigProvider: Clone + MaybeSend + MaybeSync + 'static {
    fn list_buckets(
        &self,
    ) -> impl Future<Output = Result<Vec<BucketConfig>, ProxyError>> + MaybeSend;

    fn get_bucket(
        &self,
        name: &str,
    ) -> impl Future<Output = Result<Option<BucketConfig>, ProxyError>> + MaybeSend;

    fn get_role(
        &self,
        role_id: &str,
    ) -> impl Future<Output = Result<Option<RoleConfig>, ProxyError>> + MaybeSend;

    /// Look up a long-lived credential by its access key ID.
    fn get_credential(
        &self,
        access_key_id: &str,
    ) -> impl Future<Output = Result<Option<StoredCredential>, ProxyError>> + MaybeSend;

    /// Store a temporary credential (minted by the STS API).
    fn store_temporary_credential(
        &self,
        cred: &TemporaryCredentials,
    ) -> impl Future<Output = Result<(), ProxyError>> + MaybeSend;

    /// Look up a temporary credential by its access key ID.
    fn get_temporary_credential(
        &self,
        access_key_id: &str,
    ) -> impl Future<Output = Result<Option<TemporaryCredentials>, ProxyError>> + MaybeSend;
}
