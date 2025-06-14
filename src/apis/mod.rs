pub mod source;

use crate::{backends::common::Repository, utils::auth::UserIdentity, utils::errors::BackendError};
use async_trait::async_trait;

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
pub trait Api {
    async fn get_backend_client(
        &self,
        account_id: &str,
        repository_id: &str,
    ) -> Result<Box<dyn Repository>, BackendError>;

    async fn get_account(
        &self,
        account_id: String,
        user_identity: UserIdentity,
    ) -> Result<Account, BackendError>;
}
