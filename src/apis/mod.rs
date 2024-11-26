pub mod source;

use crate::{backends::common::Repository, utils::auth::UserIdentity};
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
pub trait API {
    async fn get_backend_client(
        &self,
        account_id: &String,
        repository_id: &String,
    ) -> Result<Box<dyn Repository>, ()>;

    async fn get_account(
        &self,
        account_id: String,
        user_identity: UserIdentity,
    ) -> Result<Account, ()>;
}
