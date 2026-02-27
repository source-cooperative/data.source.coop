//! DynamoDB-backed configuration provider.
//!
//! Stores configuration in DynamoDB tables. Designed for AWS-native
//! deployments where DynamoDB is readily available.
//!
//! # Table Schema
//!
//! Uses a single-table design with the following layout:
//!
//! | PK | SK | Attributes |
//! |----|----|------------|
//! | `BUCKET#{name}` | `CONFIG` | BucketConfig fields |
//! | `ROLE#{role_id}` | `CONFIG` | RoleConfig fields |
//! | `CRED#{access_key_id}` | `LONG_LIVED` | StoredCredential fields |
//!
//! # Example
//!
//! ```rust,ignore
//! use source_coop_core::config::dynamodb::DynamoDbProvider;
//! use aws_sdk_dynamodb::Client;
//!
//! let sdk_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
//! let client = Client::new(&sdk_config);
//! let provider = DynamoDbProvider::new(client, "s3-proxy-config".to_string());
//! ```

use crate::config::ConfigProvider;
use crate::error::ProxyError;
use crate::types::{BucketConfig, RoleConfig, StoredCredential};
use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_dynamodb::Client;
use std::sync::Arc;

/// Configuration provider backed by a single DynamoDB table.
#[derive(Clone)]
pub struct DynamoDbProvider {
    inner: Arc<DynamoDbProviderInner>,
}

struct DynamoDbProviderInner {
    client: Client,
    table_name: String,
}

impl DynamoDbProvider {
    pub fn new(client: Client, table_name: String) -> Self {
        Self {
            inner: Arc::new(DynamoDbProviderInner { client, table_name }),
        }
    }

    fn table(&self) -> &str {
        &self.inner.table_name
    }

    fn client(&self) -> &Client {
        &self.inner.client
    }
}

impl ConfigProvider for DynamoDbProvider {
    async fn list_buckets(&self) -> Result<Vec<BucketConfig>, ProxyError> {
        let result = self
            .client()
            .query()
            .table_name(self.table())
            .key_condition_expression("begins_with(PK, :prefix)")
            .expression_attribute_values(":prefix", AttributeValue::S("BUCKET#".into()))
            .send()
            .await
            .map_err(|e| ProxyError::ConfigError(e.to_string()))?;

        let items = result.items();
        let mut buckets = Vec::with_capacity(items.len());

        for item in items {
            if let Some(json_val) = item.get("config_json") {
                if let Ok(s) = json_val.as_s() {
                    if let Ok(config) = serde_json::from_str::<BucketConfig>(s) {
                        buckets.push(config);
                    }
                }
            }
        }

        Ok(buckets)
    }

    async fn get_bucket(&self, name: &str) -> Result<Option<BucketConfig>, ProxyError> {
        let result = self
            .client()
            .get_item()
            .table_name(self.table())
            .key("PK", AttributeValue::S(format!("BUCKET#{}", name)))
            .key("SK", AttributeValue::S("CONFIG".into()))
            .send()
            .await
            .map_err(|e| ProxyError::ConfigError(e.to_string()))?;

        match result.item() {
            Some(item) => {
                let json_val = item
                    .get("config_json")
                    .and_then(|v| v.as_s().ok())
                    .ok_or_else(|| ProxyError::ConfigError("missing config_json".into()))?;

                let config = serde_json::from_str(json_val)
                    .map_err(|e| ProxyError::ConfigError(e.to_string()))?;
                Ok(Some(config))
            }
            None => Ok(None),
        }
    }

    async fn get_role(&self, role_id: &str) -> Result<Option<RoleConfig>, ProxyError> {
        let result = self
            .client()
            .get_item()
            .table_name(self.table())
            .key("PK", AttributeValue::S(format!("ROLE#{}", role_id)))
            .key("SK", AttributeValue::S("CONFIG".into()))
            .send()
            .await
            .map_err(|e| ProxyError::ConfigError(e.to_string()))?;

        match result.item() {
            Some(item) => {
                let json_val = item
                    .get("config_json")
                    .and_then(|v| v.as_s().ok())
                    .ok_or_else(|| ProxyError::ConfigError("missing config_json".into()))?;

                let config = serde_json::from_str(json_val)
                    .map_err(|e| ProxyError::ConfigError(e.to_string()))?;
                Ok(Some(config))
            }
            None => Ok(None),
        }
    }

    async fn get_credential(
        &self,
        access_key_id: &str,
    ) -> Result<Option<StoredCredential>, ProxyError> {
        let result = self
            .client()
            .get_item()
            .table_name(self.table())
            .key("PK", AttributeValue::S(format!("CRED#{}", access_key_id)))
            .key("SK", AttributeValue::S("LONG_LIVED".into()))
            .send()
            .await
            .map_err(|e| ProxyError::ConfigError(e.to_string()))?;

        match result.item() {
            Some(item) => {
                let json_val = item
                    .get("config_json")
                    .and_then(|v| v.as_s().ok())
                    .ok_or_else(|| ProxyError::ConfigError("missing config_json".into()))?;

                let config = serde_json::from_str(json_val)
                    .map_err(|e| ProxyError::ConfigError(e.to_string()))?;
                Ok(Some(config))
            }
            None => Ok(None),
        }
    }

}
