use actix_web::error;
use actix_web::http::StatusCode;
use actix_web::HttpResponse;
use azure_core::{
    error::{Error as AzureError, ErrorKind as AzureErrorKind},
    StatusCode as AzureStatusCode,
};
use log::error;
use quick_xml::DeError;
use reqwest::Error as ReqwestError;
use rusoto_core::RusotoError;
use rusoto_s3::{
    AbortMultipartUploadError, CompleteMultipartUploadError, CreateMultipartUploadError,
    DeleteObjectError, HeadObjectError, ListObjectsV2Error, PutObjectError, UploadPartError,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum BackendError {
    #[error("repository not found")]
    RepositoryNotFound,

    #[error("failed to fetch repository permissions")]
    RepositoryPermissionsNotFound,

    #[error("source repository missing primary mirror")]
    SourceRepositoryMissingPrimaryMirror,

    #[error("object not found: {0:?}")]
    ObjectNotFound(Option<String>),

    #[error("api key not found")]
    ApiKeyNotFound,

    #[error("data connection not found")]
    DataConnectionNotFound,

    #[error("invalid request")]
    InvalidRequest(String),

    #[error("reqwest error (url {}, message {})", .0.url().map(|u| u.to_string()).unwrap_or("unknown".to_string()), .0.to_string())]
    ReqwestError(#[from] ReqwestError),

    #[error("api threw a server error (url {}, status {}, message {})", .url, .status, .message)]
    ApiServerError {
        url: String,
        status: u16,
        message: String,
    },

    #[error("api threw a client error (url {}, status {}, message {})", .url, .status, .message)]
    ApiClientError {
        url: String,
        status: u16,
        message: String,
    },

    #[error("failed to parse JSON (url {})", .url)]
    JsonParseError { url: String },

    #[error("unexpected data connection provider (provider {})", .provider)]
    UnexpectedDataConnectionProvider { provider: String },

    #[error("unauthorized")]
    UnauthorizedError,

    #[error("unexpected API error: {0}")]
    UnexpectedApiError(String),

    #[error("unsupported auth method: {0}")]
    UnsupportedAuthMethod(String),

    #[error("unsupported operation: {0}")]
    UnsupportedOperation(String),

    #[error("xml parse error: {0}")]
    XmlParseError(String),

    #[error("azure error: {0}")]
    AzureError(AzureError),

    #[error("s3 error: {0}")]
    S3Error(String),
}

impl From<AzureError> for BackendError {
    fn from(error: AzureError) -> BackendError {
        match error.kind() {
            AzureErrorKind::HttpResponse { status, error_code }
                if *status == AzureStatusCode::NotFound =>
            {
                BackendError::ObjectNotFound(error_code.clone())
            }
            _ => BackendError::AzureError(error),
        }
    }
}

impl error::ResponseError for BackendError {
    fn error_response(&self) -> HttpResponse {
        error!("Error: {}", self);
        let status_code = self.status_code();
        let body = match status_code {
            e if e.is_client_error() => self.to_string(),
            _ => format!("Internal Server Error: {}", self.to_string()),
        };
        HttpResponse::build(status_code).body(body)
    }

    fn status_code(&self) -> StatusCode {
        match self {
            // 400
            BackendError::InvalidRequest(_)
            | BackendError::UnsupportedAuthMethod(_)
            | BackendError::UnsupportedOperation(_) => StatusCode::BAD_REQUEST,
            // 401
            BackendError::UnauthorizedError => StatusCode::UNAUTHORIZED,
            // 404
            BackendError::RepositoryNotFound
            | BackendError::ObjectNotFound(_)
            | BackendError::SourceRepositoryMissingPrimaryMirror
            | BackendError::ApiKeyNotFound
            | BackendError::DataConnectionNotFound => StatusCode::NOT_FOUND,

            // 502
            BackendError::ReqwestError(_)
            | BackendError::ApiServerError { .. }
            | BackendError::ApiClientError { .. }
            | BackendError::RepositoryPermissionsNotFound
            | BackendError::AzureError(_)
            | BackendError::S3Error(_) => StatusCode::BAD_GATEWAY,
            // 500
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

fn get_rusoto_error_message<T: std::error::Error>(
    operation: &str,
    error: RusotoError<T>,
) -> String {
    match error {
        RusotoError::Service(error) => {
            format!("{} Service Error: {}", operation, error.to_string())
        }
        RusotoError::HttpDispatch(error) => {
            format!("{} HttpDispatch Error: {}", operation, error.to_string())
        }
        RusotoError::Credentials(error) => {
            format!("{} Credentials Error: {}", operation, error.to_string())
        }
        RusotoError::Validation(error) => {
            format!("{} Validation Error: {}", operation, error.to_string())
        }
        RusotoError::ParseError(error) => {
            format!("{} Parse Error: {}", operation, error.to_string())
        }
        RusotoError::Unknown(error) => {
            format!(
                "{} Unknown Error: status {}, body {}",
                operation,
                error.status,
                error.body_as_str(),
            )
        }
        RusotoError::Blocking => format!("{} Blocking Error", operation,),
    }
}

// S3 API Errors
impl From<RusotoError<HeadObjectError>> for BackendError {
    fn from(error: RusotoError<HeadObjectError>) -> BackendError {
        match error {
            RusotoError::Service(HeadObjectError::NoSuchKey(e)) => {
                BackendError::ObjectNotFound(Some(e))
            }
            _ => BackendError::S3Error(get_rusoto_error_message("HeadObject", error)),
        }
    }
}
impl From<RusotoError<DeleteObjectError>> for BackendError {
    fn from(error: RusotoError<DeleteObjectError>) -> BackendError {
        BackendError::S3Error(get_rusoto_error_message("DeleteObject", error))
    }
}
impl From<RusotoError<PutObjectError>> for BackendError {
    fn from(error: RusotoError<PutObjectError>) -> BackendError {
        BackendError::S3Error(get_rusoto_error_message("PutObject", error))
    }
}
impl From<RusotoError<CreateMultipartUploadError>> for BackendError {
    fn from(error: RusotoError<CreateMultipartUploadError>) -> BackendError {
        BackendError::S3Error(get_rusoto_error_message("CreateMultipartUpload", error))
    }
}
impl From<RusotoError<AbortMultipartUploadError>> for BackendError {
    fn from(error: RusotoError<AbortMultipartUploadError>) -> BackendError {
        BackendError::S3Error(get_rusoto_error_message("AbortMultipartUpload", error))
    }
}
impl From<RusotoError<CompleteMultipartUploadError>> for BackendError {
    fn from(error: RusotoError<CompleteMultipartUploadError>) -> BackendError {
        BackendError::S3Error(get_rusoto_error_message("CompleteMultipartUpload", error))
    }
}
impl From<RusotoError<UploadPartError>> for BackendError {
    fn from(error: RusotoError<UploadPartError>) -> BackendError {
        BackendError::S3Error(get_rusoto_error_message("UploadPart", error))
    }
}
impl From<RusotoError<ListObjectsV2Error>> for BackendError {
    fn from(error: RusotoError<ListObjectsV2Error>) -> BackendError {
        BackendError::S3Error(get_rusoto_error_message("ListObjectsV2", error))
    }
}

impl From<DeError> for BackendError {
    fn from(error: DeError) -> BackendError {
        BackendError::XmlParseError(format!("failed to parse xml: {}", error))
    }
}
impl From<serde_xml_rs::Error> for BackendError {
    fn from(error: serde_xml_rs::Error) -> BackendError {
        BackendError::XmlParseError(format!("failed to parse xml: {}", error))
    }
}
