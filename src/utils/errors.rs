use actix_web::HttpResponse;
use quick_xml::se::to_string_with_root;
use serde::Serialize;
use std::error::Error;
use std::fmt;

use crate::backends::common::S3ErrorResponse;

use super::context::RequestContext;

pub trait APIError: std::error::Error + Send + Sync {
    fn to_response(&self) -> HttpResponse;
}

#[derive(Serialize, Debug, Clone)]
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
        write!(f, "{} {}", self.account_id, self.repository_id)
    }
}

impl Error for RepositoryNotFoundError {}

#[derive(Clone, Serialize, Debug)]
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

#[derive(Serialize, Debug, Clone)]
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

#[derive(Serialize, Debug, Clone)]
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

#[derive(Serialize, Debug, Clone)]
pub struct BadRequestError {
    pub message: String,
}

impl APIError for BadRequestError {
    fn to_response(&self) -> HttpResponse {
        let error_response = S3ErrorResponse {
            code: "AccessDenied".to_string(),
            message: self.message.clone(),
            resource: "".to_string(),
            request_id: "".to_string(),
        };
        match to_string_with_root("Error", &error_response) {
            Ok(xml) => HttpResponse::Forbidden()
                .content_type("application/xml")
                .body(xml),
            Err(_) => HttpResponse::InternalServerError().finish(),
        }
    }
}

impl fmt::Display for BadRequestError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Bad Request Error: {}", self.message)
    }
}

impl Error for BadRequestError {}
