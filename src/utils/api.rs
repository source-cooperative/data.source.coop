use crate::utils::errors::BackendError;
use reqwest::{Response, StatusCode};
use serde::de::DeserializeOwned;

/// Process a response, handling both success and error cases
pub async fn process_json_response<T: DeserializeOwned>(
    response: Response,
    not_found_error: BackendError,
) -> Result<T, BackendError> {
    let status = response.status();
    if status.is_success() {
        response
            .json::<T>()
            .await
            .map_err(|err| BackendError::JsonParseError {
                url: err
                    .url()
                    .map(|u| u.to_string())
                    .unwrap_or("unknown".to_string()),
            })
    } else if status == StatusCode::NOT_FOUND {
        Err(not_found_error)
    } else {
        let url = response.url().to_string();
        let status = response.status().as_u16();
        let is_server_error = response.status().is_server_error();
        let message = response.text().await.unwrap_or("unknown".to_string());

        if is_server_error {
            Err(BackendError::ApiServerError {
                url,
                status,
                message,
            })
        } else {
            Err(BackendError::ApiClientError {
                url,
                status,
                message,
            })
        }
    }
}
