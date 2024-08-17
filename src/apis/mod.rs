pub mod source;

use crate::backends::common::Repository;
use async_trait::async_trait;

pub fn new_api() -> source::SourceAPI {
    source::SourceAPI {
        endpoint: "https://api.source.coop".to_string(),
    }
}

#[async_trait]
pub trait API {
    async fn get_backend_client(
        &self,
        account_id: String,
        repository_id: String,
    ) -> Result<Box<dyn Repository>, ()>;
}
