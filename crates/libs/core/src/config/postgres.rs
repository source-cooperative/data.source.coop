//! PostgreSQL-backed configuration provider.
//!
//! Stores configuration in a Postgres database. Good for deployments where
//! you already have a Postgres instance and want transactional config updates.
//!
//! # Required Tables
//!
//! ```sql
//! CREATE TABLE proxy_buckets (
//!     name TEXT PRIMARY KEY,
//!     config_json JSONB NOT NULL
//! );
//!
//! CREATE TABLE proxy_roles (
//!     role_id TEXT PRIMARY KEY,
//!     config_json JSONB NOT NULL
//! );
//!
//! CREATE TABLE proxy_credentials (
//!     access_key_id TEXT PRIMARY KEY,
//!     config_json JSONB NOT NULL
//! );
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use source_coop_core::config::postgres::PostgresProvider;
//! use sqlx::PgPool;
//!
//! let pool = PgPool::connect("postgres://user:pass@localhost/s3proxy").await?;
//! let provider = PostgresProvider::new(pool);
//! ```

use crate::config::ConfigProvider;
use crate::error::ProxyError;
use crate::types::{BucketConfig, RoleConfig, StoredCredential};
use sqlx::PgPool;
use std::sync::Arc;

/// Configuration provider backed by PostgreSQL.
#[derive(Clone)]
pub struct PostgresProvider {
    pool: Arc<PgPool>,
}

impl PostgresProvider {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool: Arc::new(pool),
        }
    }
}

impl ConfigProvider for PostgresProvider {
    async fn list_buckets(&self) -> Result<Vec<BucketConfig>, ProxyError> {
        let rows: Vec<(serde_json::Value,)> =
            sqlx::query_as("SELECT config_json FROM proxy_buckets")
                .fetch_all(self.pool.as_ref())
                .await
                .map_err(|e| ProxyError::ConfigError(e.to_string()))?;

        rows.into_iter()
            .map(|(json,)| {
                serde_json::from_value(json).map_err(|e| ProxyError::ConfigError(e.to_string()))
            })
            .collect()
    }

    async fn get_bucket(&self, name: &str) -> Result<Option<BucketConfig>, ProxyError> {
        let row: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT config_json FROM proxy_buckets WHERE name = $1")
                .bind(name)
                .fetch_optional(self.pool.as_ref())
                .await
                .map_err(|e| ProxyError::ConfigError(e.to_string()))?;

        row.map(|(json,)| {
            serde_json::from_value(json).map_err(|e| ProxyError::ConfigError(e.to_string()))
        })
        .transpose()
    }

    async fn get_role(&self, role_id: &str) -> Result<Option<RoleConfig>, ProxyError> {
        let row: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT config_json FROM proxy_roles WHERE role_id = $1")
                .bind(role_id)
                .fetch_optional(self.pool.as_ref())
                .await
                .map_err(|e| ProxyError::ConfigError(e.to_string()))?;

        row.map(|(json,)| {
            serde_json::from_value(json).map_err(|e| ProxyError::ConfigError(e.to_string()))
        })
        .transpose()
    }

    async fn get_credential(
        &self,
        access_key_id: &str,
    ) -> Result<Option<StoredCredential>, ProxyError> {
        let row: Option<(serde_json::Value,)> = sqlx::query_as(
            "SELECT config_json FROM proxy_credentials
             WHERE access_key_id = $1 AND credential_type = 'long_lived'",
        )
        .bind(access_key_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| ProxyError::ConfigError(e.to_string()))?;

        row.map(|(json,)| {
            serde_json::from_value(json).map_err(|e| ProxyError::ConfigError(e.to_string()))
        })
        .transpose()
    }
}
