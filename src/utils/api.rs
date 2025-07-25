use crate::utils::errors::BackendError;
use reqwest::{Response, StatusCode};
use serde::de::DeserializeOwned;

/// Process a response, handling both success and error cases
pub async fn process_json_response<T: DeserializeOwned>(
    response: Response,
    not_found_error: BackendError,
) -> Result<T, BackendError> {
    let status = response.status();
    let url = response.url().to_string();
    let text = response
        .text()
        .await
        .unwrap_or_else(|_| "<failed to read body>".to_string());

    if status.is_success() {
        match serde_json::from_str::<T>(&text) {
            Ok(val) => Ok(val),
            Err(err) => {
                log::error!("Failed to parse JSON from {}: {}\nBody: {}", url, err, text);
                Err(BackendError::JsonParseError { url })
            }
        }
    } else if status == StatusCode::NOT_FOUND {
        Err(not_found_error)
    } else {
        let is_server_error = status.is_server_error();
        if is_server_error {
            log::error!("Server error from {}: {}\nBody: {}", url, status, text);
            Err(BackendError::ApiServerError {
                url,
                status: status.as_u16(),
                message: text,
            })
        } else {
            log::warn!("Client error from {}: {}\nBody: {}", url, status, text);
            Err(BackendError::ApiClientError {
                url,
                status: status.as_u16(),
                message: text,
            })
        }
    }
}
