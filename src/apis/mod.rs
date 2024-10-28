pub mod source;

use crate::backends::common::Repository;
use crate::utils::context::RequestContext;
use crate::utils::errors::APIError;
use async_trait::async_trait;
use std::sync::Arc;

pub struct Account {
    pub repositories: Vec<String>,
}

impl Account {
    fn default() -> Account {
        Account {
            repositories: Vec::new(),
        }
    }
}

#[async_trait]
pub trait API {
    async fn get_backend_client(
        &self,
        ctx: &RequestContext,
    ) -> Result<Arc<dyn Repository>, Arc<dyn APIError>>;

    async fn get_account(&self, account_id: String) -> Result<Account, ()>;
}
