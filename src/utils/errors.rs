use actix_web::HttpResponse;
use serde::Serialize;
use std::error::Error;
use std::fmt;

pub trait APIError: std::error::Error + Send + Sync {
    fn to_response(&self) -> HttpResponse;
}

#[derive(Serialize, Debug)]
pub struct RepositoryNotFoundError {
    pub account_id: String,
    pub repository_id: String,
}

impl APIError for RepositoryNotFoundError {
    fn to_response(&self) -> HttpResponse {
        HttpResponse::NotFound().json(self)
    }
}

impl fmt::Display for RepositoryNotFoundError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Repository Not Found: {}/{}",
            self.account_id, self.repository_id
        )
    }
}

impl Error for RepositoryNotFoundError {}

#[derive(Serialize, Debug)]
pub struct ObjectNotFoundError {
    pub account_id: String,
    pub repository_id: String,
    pub key: String,
}

impl APIError for ObjectNotFoundError {
    fn to_response(&self) -> HttpResponse {
        HttpResponse::NotFound().json(self)
    }
}

impl fmt::Display for ObjectNotFoundError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Object Not Found: {}", self.key)
    }
}

impl Error for ObjectNotFoundError {}

#[derive(Serialize, Debug)]
pub struct AccountNotFoundError {
    pub account_id: String,
}

impl APIError for AccountNotFoundError {
    fn to_response(&self) -> HttpResponse {
        HttpResponse::NotFound().json(self)
    }
}

impl fmt::Display for AccountNotFoundError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Account Not Found: {}", self.account_id)
    }
}

impl Error for AccountNotFoundError {}

#[derive(Serialize, Debug)]
pub struct InternalServerError {
    pub message: String,
}

impl APIError for InternalServerError {
    fn to_response(&self) -> HttpResponse {
        HttpResponse::InternalServerError().json(self)
    }
}

impl fmt::Display for InternalServerError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Internal Server Error: {}", self.message)
    }
}

impl Error for InternalServerError {}
