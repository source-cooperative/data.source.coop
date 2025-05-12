use actix_web::error;
use actix_web::http::StatusCode;
use actix_web::HttpResponse;
use log::error;
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
    #[error("Unauthorized")]
    UnauthorizationError,
    #[error("Unexpected API error: {0}")] // TODO: remove this
    UnexpectedApiError(String),
}

impl error::ResponseError for BackendError {
    fn error_response(&self) -> HttpResponse {
        error!("Error: {}", self);
        match self {
            BackendError::RepositoryNotFound => HttpResponse::NotFound().finish(),
            BackendError::SourceRepositoryMissingPrimaryMirror => HttpResponse::NotFound().finish(),
            BackendError::DataConnectionNotFound => HttpResponse::NotFound().finish(),
            BackendError::ReqwestError(_) => HttpResponse::BadGateway().finish(),
            BackendError::ApiServerError { .. } => HttpResponse::BadGateway().finish(),
            BackendError::ApiClientError { .. } => HttpResponse::BadGateway().finish(),
            BackendError::JsonParseError { .. } => HttpResponse::InternalServerError().finish(),
            BackendError::UnexpectedDataConnectionProvider { .. } => {
                HttpResponse::InternalServerError().finish()
            }
            BackendError::RepositoryPermissionsNotFound => HttpResponse::BadGateway().finish(),
            BackendError::UnauthorizationError => HttpResponse::Unauthorized().finish(),
            BackendError::UnexpectedApiError(_) => HttpResponse::InternalServerError().finish(),
        }
    }

    fn status_code(&self) -> StatusCode {
        match self {
            BackendError::RepositoryNotFound => StatusCode::NOT_FOUND,
            BackendError::SourceRepositoryMissingPrimaryMirror => StatusCode::NOT_FOUND,
            BackendError::DataConnectionNotFound => StatusCode::NOT_FOUND,
            BackendError::ReqwestError(_) => StatusCode::BAD_GATEWAY,
            BackendError::ApiServerError { .. } => StatusCode::BAD_GATEWAY,
            BackendError::ApiClientError { .. } => StatusCode::BAD_GATEWAY,
            BackendError::JsonParseError { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            BackendError::UnexpectedDataConnectionProvider { .. } => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            BackendError::RepositoryPermissionsNotFound => StatusCode::BAD_GATEWAY,
            BackendError::UnauthorizationError => StatusCode::UNAUTHORIZED,
            BackendError::UnexpectedApiError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<Box<dyn APIError>> for BackendError {
    fn from(error: Box<dyn APIError>) -> BackendError {
        BackendError::UnexpectedApiError(error.to_string())
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
