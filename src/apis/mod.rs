pub mod source;

use crate::backends::common::Repository;
use async_trait::async_trait;

pub fn new_api() -> source::SourceAPI {
    source::SourceAPI {
        endpoint: "https://api.source.coop".to_string(),
    }
}

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
        account_id: String,
        repository_id: String,
    ) -> Result<Box<dyn Repository>, ()>;

    async fn get_account(&self, account_id: String) -> Result<Account, ()>;
}
