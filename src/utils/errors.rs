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
    ObjectNotFound(String),

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

impl error::ResponseError for BackendError {
    fn error_response(&self) -> HttpResponse {
        let status_code = self.status_code();
        let body = match status_code {
            e if e.is_client_error() => self.to_string(),
            _ => format!("Internal Server Error: {}", self.to_string()),
        };
        if status_code.is_server_error() {
            error!("Error: {}", self);
        }
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

// Azure API Errors
impl From<AzureError> for BackendError {
    fn from(error: AzureError) -> BackendError {
        match error.kind() {
            AzureErrorKind::HttpResponse { status, error_code }
                if *status == AzureStatusCode::NotFound =>
            {
                BackendError::ObjectNotFound(error_code.clone().unwrap_or("".to_string()))
            }
            _ => BackendError::AzureError(error),
        }
    }
}

// S3 API Errors
fn get_rusoto_error_message<T: std::error::Error>(
    operation: &str,
    error: RusotoError<T>,
) -> String {
    match error {
        RusotoError::Service(e) => format!("{} Service Error: {}", operation, e),
        RusotoError::HttpDispatch(e) => format!("{} HttpDispatch Error: {}", operation, e),
        RusotoError::Credentials(e) => format!("{} Credentials Error: {}", operation, e),
        RusotoError::Validation(e) => format!("{} Validation Error: {}", operation, e),
        RusotoError::ParseError(e) => format!("{} Parse Error: {}", operation, e),
        RusotoError::Unknown(e) => format!("{} Unknown Error: status {}", operation, e.status),
        RusotoError::Blocking => format!("{} Blocking Error", operation),
    }
}
macro_rules! impl_s3_errors {
    ($(($error_type:ty, $operation:expr)),* $(,)?) => {
        $(
            impl From<RusotoError<$error_type>> for BackendError {
                fn from(error: RusotoError<$error_type>) -> BackendError {
                    BackendError::S3Error(get_rusoto_error_message($operation, error))
                }
            }
        )*
    };
}
impl_s3_errors!(
    (DeleteObjectError, "DeleteObject"),
    (PutObjectError, "PutObject"),
    (CreateMultipartUploadError, "CreateMultipartUpload"),
    (AbortMultipartUploadError, "AbortMultipartUpload"),
    (CompleteMultipartUploadError, "CompleteMultipartUpload"),
    (UploadPartError, "UploadPart"),
);
impl From<RusotoError<HeadObjectError>> for BackendError {
    fn from(error: RusotoError<HeadObjectError>) -> BackendError {
        match error {
            RusotoError::Service(HeadObjectError::NoSuchKey(e)) => BackendError::ObjectNotFound(e),
            RusotoError::Unknown(e) if e.status == StatusCode::NOT_FOUND => {
                BackendError::ObjectNotFound(e.body_as_str().to_string())
            }
            _ => BackendError::S3Error(get_rusoto_error_message("HeadObject", error)),
        }
    }
}
impl From<RusotoError<ListObjectsV2Error>> for BackendError {
    fn from(error: RusotoError<ListObjectsV2Error>) -> BackendError {
        match error {
            RusotoError::Service(ListObjectsV2Error::NoSuchBucket(_)) => {
                BackendError::RepositoryNotFound
            }
            _ => BackendError::S3Error(get_rusoto_error_message("ListObjectsV2", error)),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::body::to_bytes;
    use actix_web::error::ResponseError;
    use actix_web::http::StatusCode;
    use bytes::Bytes;
    use quick_xml::DeError;
    use rusoto_core::RusotoError;
    use rusoto_s3::{HeadObjectError, ListObjectsV2Error, PutObjectError};
    use serde_xml_rs::Error as XmlError;

    /// Tests for S3 error handling
    mod s3_errors {
        use super::*;

        #[tokio::test]
        async fn should_convert_head_object_no_such_key_to_404() {
            let error = RusotoError::Service(HeadObjectError::NoSuchKey("test-key".to_string()));
            let backend_error = BackendError::from(error);

            assert!(
                matches!(backend_error, BackendError::ObjectNotFound(_)),
                "expected error to be ObjectNotFound"
            );
            assert_eq!(
                backend_error.status_code(),
                StatusCode::NOT_FOUND,
                "expected status code to be 404"
            );
            let response = backend_error.error_response();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert_eq!(
                to_bytes(response.into_body()).await.unwrap(),
                Bytes::from("object not found: \"test-key\"")
            );
        }

        #[tokio::test]
        async fn should_convert_list_objects_no_such_bucket_to_404() {
            let error =
                RusotoError::Service(ListObjectsV2Error::NoSuchBucket("test-bucket".to_string()));
            let backend_error = BackendError::from(error);

            assert!(
                matches!(backend_error, BackendError::RepositoryNotFound),
                "expected error to be converted to RepositoryNotFound"
            );
            assert_eq!(
                backend_error.status_code(),
                StatusCode::NOT_FOUND,
                "expected status code to be 404"
            );
            let response = backend_error.error_response();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert_eq!(
                to_bytes(response.into_body()).await.unwrap(),
                Bytes::from("repository not found")
            );
        }

        #[tokio::test]
        async fn should_convert_put_object_unknown_error_to_502() {
            let error: RusotoError<PutObjectError> =
                RusotoError::Unknown(rusoto_core::request::BufferedHttpResponse {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    headers: Default::default(),
                    body: Bytes::new(),
                });
            let backend_error = BackendError::from(error);

            assert!(
                matches!(backend_error, BackendError::S3Error(_)),
                "expected error to be converted to S3Error"
            );
            assert_eq!(
                backend_error.status_code(),
                StatusCode::BAD_GATEWAY,
                "expected status code to be 502"
            );
            let response = backend_error.error_response();
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            assert_eq!(
                to_bytes(response.into_body()).await.unwrap(),
                Bytes::from("Internal Server Error: s3 error: PutObject Unknown Error: status 500 Internal Server Error")
            );
        }
    }

    /// Tests for Azure error handling
    mod azure_errors {
        use super::*;

        #[tokio::test]
        async fn should_convert_not_found_to_404() {
            let error = AzureError::new(
                AzureErrorKind::HttpResponse {
                    status: AzureStatusCode::NotFound,
                    error_code: Some("ResourceNotFound".to_string()),
                },
                "Resource not found",
            );
            let backend_error = BackendError::from(error);

            assert!(
                matches!(backend_error, BackendError::ObjectNotFound(_)),
                "expected error to be converted to ObjectNotFound"
            );
            assert_eq!(
                backend_error.status_code(),
                StatusCode::NOT_FOUND,
                "expected status code to be 404"
            );
            let response = backend_error.error_response();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert_eq!(
                to_bytes(response.into_body()).await.unwrap(),
                Bytes::from("object not found: \"ResourceNotFound\"")
            );
        }

        #[tokio::test]
        async fn should_convert_other_errors_to_502() {
            let error = AzureError::new(
                AzureErrorKind::HttpResponse {
                    status: AzureStatusCode::InternalServerError,
                    error_code: Some("InternalError".to_string()),
                },
                "Internal error",
            );
            let backend_error = BackendError::from(error);

            assert!(
                matches!(backend_error, BackendError::AzureError(_)),
                "expected error to be converted to AzureError"
            );
            assert_eq!(
                backend_error.status_code(),
                StatusCode::BAD_GATEWAY,
                "expected status code to be 502"
            );
            let response = backend_error.error_response();
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            assert_eq!(
                to_bytes(response.into_body()).await.unwrap(),
                Bytes::from("Internal Server Error: azure error: Internal error")
            );
        }
    }

    /// Tests for client-side error handling
    mod client_errors {
        use super::*;

        #[tokio::test]
        async fn should_handle_unauthorized_error() {
            let error = BackendError::UnauthorizedError;
            assert_eq!(
                error.status_code(),
                StatusCode::UNAUTHORIZED,
                "expected status code to be 401"
            );
            assert_eq!(
                error.to_string(),
                "unauthorized",
                "expected error message to be 'unauthorized'"
            );
            let response = error.error_response();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                to_bytes(response.into_body()).await.unwrap(),
                Bytes::from("unauthorized")
            );
        }

        #[tokio::test]
        async fn should_handle_invalid_request_error() {
            let error = BackendError::InvalidRequest("bad input".to_string());
            assert_eq!(
                error.status_code(),
                StatusCode::BAD_REQUEST,
                "expected status code to be 400"
            );
            assert_eq!(
                error.to_string(),
                "invalid request",
                "expected error message to be 'invalid request'"
            );
            let response = error.error_response();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(
                to_bytes(response.into_body()).await.unwrap(),
                Bytes::from("invalid request")
            );
        }

        #[tokio::test]
        async fn should_handle_unsupported_auth_method() {
            let error = BackendError::UnsupportedAuthMethod("basic".to_string());
            assert_eq!(
                error.status_code(),
                StatusCode::BAD_REQUEST,
                "expected status code to be 400"
            );
            assert_eq!(
                error.to_string(),
                "unsupported auth method: basic",
                "expected error message to include auth method"
            );
            let response = error.error_response();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(
                to_bytes(response.into_body()).await.unwrap(),
                Bytes::from("unsupported auth method: basic")
            );
        }

        #[tokio::test]
        async fn should_handle_unsupported_operation() {
            let error = BackendError::UnsupportedOperation("delete".to_string());
            assert_eq!(
                error.status_code(),
                StatusCode::BAD_REQUEST,
                "expected status code to be 400"
            );
            assert_eq!(
                error.to_string(),
                "unsupported operation: delete",
                "expected error message to include operation"
            );
            let response = error.error_response();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(
                to_bytes(response.into_body()).await.unwrap(),
                Bytes::from("unsupported operation: delete")
            );
        }
    }

    /// Tests for XML parsing errors
    mod xml_errors {
        use super::*;

        #[test]
        fn should_convert_quick_xml_error() {
            let error = DeError::UnexpectedStart(b"unexpected start of stream".to_vec());
            let backend_error = BackendError::from(error);

            assert!(
                matches!(backend_error, BackendError::XmlParseError(_)),
                "expected error to be converted to XmlParseError"
            );
            assert_eq!(
                backend_error.status_code(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "expected status code to be 500"
            );
        }

        #[test]
        fn should_convert_serde_xml_error() {
            let error = XmlError::Custom {
                field: "invalid XML format".to_string(),
            };
            let backend_error = BackendError::from(error);

            assert!(
                matches!(backend_error, BackendError::XmlParseError(_)),
                "expected error to be converted to XmlParseError"
            );
            assert_eq!(
                backend_error.status_code(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "expected status code to be 500"
            );
        }
    }

    /// Tests for API-related errors
    mod api_errors {
        use super::*;

        #[test]
        fn should_handle_api_server_error() {
            let error = BackendError::ApiServerError {
                url: "https://api.example.com".to_string(),
                status: 500,
                message: "Internal Server Error".to_string(),
            };
            assert_eq!(
                error.status_code(),
                StatusCode::BAD_GATEWAY,
                "expected status code to be 502"
            );
            assert!(
                error.to_string().contains("api threw a server error"),
                "expected error message to mention server error"
            );
        }

        #[test]
        fn should_handle_api_client_error() {
            let error = BackendError::ApiClientError {
                url: "https://api.example.com".to_string(),
                status: 400,
                message: "Bad Request".to_string(),
            };
            assert_eq!(
                error.status_code(),
                StatusCode::BAD_GATEWAY,
                "expected status code to be 502"
            );
            assert!(
                error.to_string().contains("api threw a client error"),
                "expected error message to mention client error"
            );
        }

        #[test]
        fn should_handle_json_parse_error() {
            let error = BackendError::JsonParseError {
                url: "https://api.example.com".to_string(),
            };
            assert_eq!(
                error.status_code(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "expected status code to be 500"
            );
            assert!(
                error.to_string().contains("failed to parse JSON"),
                "expected error message to mention JSON parsing"
            );
        }
    }

    /// Tests for repository-related errors
    mod repository_errors {
        use super::*;

        #[test]
        fn should_handle_repository_not_found() {
            let error = BackendError::RepositoryNotFound;
            assert_eq!(
                error.status_code(),
                StatusCode::NOT_FOUND,
                "expected status code to be 404"
            );
            assert_eq!(
                error.to_string(),
                "repository not found",
                "expected error message to be 'repository not found'"
            );
        }

        #[test]
        fn should_handle_repository_permissions_not_found() {
            let error = BackendError::RepositoryPermissionsNotFound;
            assert_eq!(
                error.status_code(),
                StatusCode::BAD_GATEWAY,
                "expected status code to be 502"
            );
            assert_eq!(
                error.to_string(),
                "failed to fetch repository permissions",
                "expected error message to mention permissions"
            );
        }

        #[test]
        fn should_handle_source_repository_missing_primary_mirror() {
            let error = BackendError::SourceRepositoryMissingPrimaryMirror;
            assert_eq!(
                error.status_code(),
                StatusCode::NOT_FOUND,
                "expected status code to be 404"
            );
            assert_eq!(
                error.to_string(),
                "source repository missing primary mirror",
                "expected error message to mention missing mirror"
            );
        }
    }

    /// Tests for data connection errors
    mod data_connection_errors {
        use super::*;

        #[test]
        fn should_handle_data_connection_not_found() {
            let error = BackendError::DataConnectionNotFound;
            assert_eq!(
                error.status_code(),
                StatusCode::NOT_FOUND,
                "expected status code to be 404"
            );
            assert_eq!(
                error.to_string(),
                "data connection not found",
                "expected error message to be 'data connection not found'"
            );
        }

        #[test]
        fn should_handle_unexpected_data_connection_provider() {
            let error = BackendError::UnexpectedDataConnectionProvider {
                provider: "unknown".to_string(),
            };
            assert_eq!(
                error.status_code(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "expected status code to be 500"
            );
            assert!(
                error
                    .to_string()
                    .contains("unexpected data connection provider"),
                "expected error message to mention unexpected provider"
            );
        }
    }
}
