use actix_web::HttpResponse;
use reqwest::Error as ReqwestError;
use serde::Serialize;
use std::error::Error;
use std::fmt;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum BackendError {
    #[error("repository not found")]
    RepositoryNotFound,
    #[error("failed to fetch repository permissions")]
    RepositoryPermissionsNotFound,
    #[error("source repository missing primary mirror")]
    SourceRepositoryMissingPrimaryMirror,
    #[error("data connection not found")]
    DataConnectionNotFound,
    #[error("reqwest error (url {}, message {})", .0.url().map(|u| u.to_string()).unwrap_or("unknown".to_string()), .0.to_string())]
    ReqwestError(#[from] ReqwestError),
    #[error("Api threw a server error (url {}, status {}, message {})", .url, .status, .message)]
    ApiServerError {
        url: String,
        status: u16,
        message: String,
    },
    #[error("Api threw a client error (url {}, status {}, message {})", .url, .status, .message)]
    ApiClientError {
        url: String,
        status: u16,
        message: String,
    },
    #[error("Failed to parse JSON (url {})", .url)]
    JsonParseError { url: String },
    #[error("Unexpected data connection provider (provider {})", .provider)]
    UnexpectedDataConnectionProvider { provider: String },
}

impl From<BackendError> for HttpResponse {
    fn from(error: BackendError) -> HttpResponse {
        match error {
            BackendError::RepositoryNotFound => HttpResponse::NotFound().finish(),
            BackendError::SourceRepositoryMissingPrimaryMirror => HttpResponse::NotFound().finish(),
            BackendError::DataConnectionNotFound => HttpResponse::NotFound().finish(),
            BackendError::ReqwestError(_e) => HttpResponse::BadGateway().finish(),
            BackendError::ApiServerError {
                url: _url,
                status: _status,
                message: _message,
            } => HttpResponse::BadGateway().finish(),
            BackendError::ApiClientError {
                url: _url,
                status: _status,
                message,
            } => HttpResponse::BadGateway().body(format!("{}", message)),
            BackendError::JsonParseError { url: _url } => {
                HttpResponse::InternalServerError().finish()
            }
            BackendError::UnexpectedDataConnectionProvider {
                provider: _provider,
            } => HttpResponse::InternalServerError().finish(),
            BackendError::RepositoryPermissionsNotFound => HttpResponse::BadGateway().finish(), // _ => HttpResponse::InternalServerError().finish(),
        }
    }
}

pub trait APIError: std::error::Error + Send + Sync {
    fn to_response(&self) -> HttpResponse;
}

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
