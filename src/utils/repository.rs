use serde::{Deserialize, Serialize};
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
