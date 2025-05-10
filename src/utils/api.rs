use crate::utils::errors::BackendError;
use reqwest::{Response, StatusCode};
use serde::de::DeserializeOwned;

/// Handle a successful response by deserializing it into the expected type
pub async fn handle_success<T: DeserializeOwned>(response: Response) -> Result<T, BackendError> {
    match response.json::<T>().await {
        Ok(data) => Ok(data),
        Err(err) => Err(BackendError::JsonParseError {
            url: err
                .url()
                .map(|u| u.to_string())
                .unwrap_or("unknown".to_string()),
        }),
    }
}

/// Handle an error response by converting it to the appropriate BackendError
pub async fn handle_error(response: Response) -> BackendError {
    let url = response.url().to_string();
    let status = response.status().as_u16();
    let is_server_error = response.status().is_server_error();
    let message = response.text().await.unwrap_or("unknown".to_string());

    if is_server_error {
        BackendError::ApiServerError {
            url,
            status,
            message,
        }
    } else {
        BackendError::ApiClientError {
            url,
            status,
            message,
        }
    }
}

/// Process a response, handling both success and error cases
pub async fn process_json_response<T: DeserializeOwned>(
    response: Response,
    not_found_error: BackendError,
) -> Result<T, BackendError> {
    let status = response.status();
    if status.is_success() {
        handle_success(response).await
    } else if status == StatusCode::NOT_FOUND {
        Err(not_found_error)
    } else {
        Err(handle_error(response).await)
    }
}
