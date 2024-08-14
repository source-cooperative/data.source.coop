use rusoto_core::Region;
use rusoto_dynamodb::{AttributeValue, DynamoDb, DynamoDbClient, GetItemInput};
use serde::{Deserialize, Serialize};
use serde_dynamodb;
use std::collections::HashMap;

#[derive(Serialize, Deserialize)]
pub struct SourceRepository {
    pub account_id: String,
    pub repository_id: String,
    pub data_mode: String,
    pub disabled: bool,
    pub featured: u8,
    pub mode: String,
    pub meta: SourceRepositoryMeta,
    pub data: SourceRepositoryData,
}

#[derive(Serialize, Deserialize)]
pub struct SourceRepositoryMeta {
    pub description: String,
    pub published: String,
    pub title: String,
    pub tags: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct SourceRepositoryData {
    pub cdn: String,
    pub primary_mirror: String,
    pub mirrors: HashMap<String, SourceRepositoryMirror>,
}

#[derive(Serialize, Deserialize)]
pub struct SourceRepositoryMirror {
    pub name: String,
    pub provider: String,
    pub region: Option<String>,
    pub uri: Option<String>,
    pub delimiter: Option<String>,
}

pub async fn get_repository_record(
    account_id: &String,
    repository_id: &String,
) -> Result<SourceRepository, ()> {
    let client = DynamoDbClient::new(Region::default());

    let mut key = HashMap::new();
    key.insert(
        "account_id".to_string(),
        AttributeValue {
            s: Some(account_id.to_string()),
            ..Default::default()
        },
    );
    key.insert(
        "repository_id".to_string(),
        AttributeValue {
            s: Some(repository_id.to_string()),
            ..Default::default()
        },
    );

    let input = GetItemInput {
        table_name: "source-cooperative-repositories".to_string(),
        key,
        ..Default::default()
    };

    match client.get_item(input).await {
        Ok(output) => {
            if let Some(item) = output.item {
                match serde_dynamodb::from_hashmap(item) {
                    Ok(repository) => {
                        return Ok(repository);
                    }
                    Err(_) => {
                        return Err(());
                    }
                }
            } else {
                Err(())
            }
        }
        Err(_) => {
            // println!("No item found with the key {:?}", key);
            Err(())
        }
    }
}
