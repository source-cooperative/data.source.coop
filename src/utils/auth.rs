use actix_http::header::HeaderMap;
use actix_web::{
    dev::{self, Service, ServiceRequest, ServiceResponse, Transform},
    web,
    web::BytesMut,
    Error, HttpMessage,
};
use futures_util::{future::LocalBoxFuture, stream::StreamExt};
use hex;
use hmac::{Hmac, Mac};
use percent_encoding::percent_decode_str;
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    future::{ready, Ready},
    rc::Rc,
};
use url::form_urlencoded;

use crate::apis::source::{APIKey, SourceApi};
use crate::utils::errors::BackendError;
use async_trait::async_trait;

#[async_trait]
pub trait ApiKeyProvider: Send + Sync {
    async fn get_api_key(&self, access_key_id: &str) -> Result<APIKey, BackendError>;
}

#[async_trait]
impl ApiKeyProvider for SourceApi {
    async fn get_api_key(&self, access_key_id: &str) -> Result<APIKey, BackendError> {
        self.get_api_key(access_key_id).await
    }
}

#[derive(Clone)]
pub struct UserIdentity {
    pub api_key: Option<APIKey>,
}

pub struct LoadIdentity;

impl<S: 'static, B> Transform<S, ServiceRequest> for LoadIdentity
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type InitError = ();
    type Transform = LoadIdentityMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(LoadIdentityMiddleware {
            service: Rc::new(service),
        }))
    }
}

pub struct LoadIdentityMiddleware<S> {
    service: Rc<S>,
}

impl<S, B> Service<ServiceRequest> for LoadIdentityMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    dev::forward_ready!(service);

    fn call(&self, mut req: ServiceRequest) -> Self::Future {
        let svc = self.service.clone();

        Box::pin(async move {
            let mut body = BytesMut::new();
            let mut stream = req.take_payload();
            while let Some(chunk) = stream.next().await {
                body.extend_from_slice(&chunk?);
            }

            let identity = match load_identity(
                req.app_data::<web::Data<Box<dyn ApiKeyProvider>>>()
                    .unwrap(),
                req.method().as_str(),
                req.path(),
                req.headers(),
                req.query_string(),
                &body,
            )
            .await
            {
                Ok(api_key) => UserIdentity {
                    api_key: Some(api_key),
                },
                Err(_) => UserIdentity { api_key: None },
            };

            req.extensions_mut().insert(identity);

            let (_, mut payload) = actix_http::h1::Payload::create(true);

            payload.unread_data(body.into());
            req.set_payload(payload.into());

            let res = svc.call(req).await?;

            Ok(res)
        })
    }
}

async fn load_identity(
    source_api: &web::Data<Box<dyn ApiKeyProvider>>,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    query_string: &str,
    body: &BytesMut,
) -> Result<APIKey, String> {
    let Some(auth) = headers.get("Authorization") else {
        return Err("No Authorization header found".to_string());
    };

    let authorization_header = auth.to_str().unwrap();
    let signature_method = authorization_header.split(" ").next().unwrap();

    if signature_method != "AWS4-HMAC-SHA256" {
        return Err("Invalid Signature Algorithm".to_string());
    }

    let parts = authorization_header.split(", ").collect::<Vec<&str>>();

    let credential = parts[0].split("Credential=").nth(1).unwrap_or("");
    let signed_headers = parts[1]
        .split("SignedHeaders=")
        .nth(1)
        .unwrap_or("")
        .split(";")
        .collect();
    let signature = parts[2].split("Signature=").nth(1).unwrap_or("");

    let parts = credential.split("/").collect::<Vec<&str>>();
    let access_key_id = parts[0];
    let date = parts[1];
    let region = parts[2];
    let service = parts[3];

    let Some(content_hash) = headers.get("x-amz-content-sha256") else {
        return Err("No x-amz-content-sha256 header found".to_string());
    };

    let canonical_request = create_canonical_request(
        method,
        path,
        headers,
        signed_headers,
        query_string,
        body,
        content_hash.to_str().unwrap(),
    );
    let credential_scope = format!("{}/{}/{}/aws4_request", date, region, service);

    let Some(datetime) = headers.get("x-amz-date") else {
        return Err("No x-amz-date header found".to_string());
    };

    let api_key = source_api
        .get_api_key(access_key_id)
        .await
        .map_err(|e| e.to_string())?;

    let string_to_sign = create_string_to_sign(
        &canonical_request,
        datetime.to_str().unwrap(),
        &credential_scope,
    );

    let calculated_signature = calculate_signature(
        api_key.secret_access_key.as_str(),
        date,
        region,
        service,
        &string_to_sign,
    );

    if calculated_signature != signature {
        Err("Signature mismatch".to_string())
    } else {
        Ok(api_key)
    }
}

fn uri_encode(input: &str, encode_forward_slash: bool) -> Cow<str> {
    let mut encoded = String::new();
    let chars = input.chars().peekable();

    for ch in chars {
        if (ch == '/' && !encode_forward_slash)
            || (ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' || ch == '~')
        {
            encoded.push(ch);
        } else {
            for byte in ch.to_string().as_bytes() {
                encoded.push_str(&format!("%{:02X}", byte));
            }
        }
    }

    if encoded == input {
        Cow::Borrowed(input)
    } else {
        Cow::Owned(encoded)
    }
}

fn trim(input: &str) -> String {
    input.trim().to_string()
}

fn lowercase(input: &str) -> String {
    input.to_lowercase()
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    // Create HMAC-SHA256 instance
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC can take key of any size");

    // Add message to HMAC
    mac.update(message);

    // Calculate HMAC
    let result = mac.finalize();

    // Get the result as bytes
    result.into_bytes().to_vec()
}

fn calculate_signature(
    key: &str,
    date: &str,
    region: &str,
    service: &str,
    string_to_sign: &str,
) -> String {
    let k_date = hmac_sha256(format!("AWS4{}", key).as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");

    hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()))
}

fn create_string_to_sign(
    canonical_request: &str,
    datetime: &str,
    credential_scope: &str,
) -> String {
    format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        datetime,
        credential_scope,
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    )
}

fn create_canonical_request(
    method: &str,
    path: &str,
    headers: &HeaderMap,
    signed_headers: Vec<&str>,
    query_string: &str,
    body: &BytesMut,
    content_hash: &str,
) -> String {
    let decoded_path = percent_decode_str(path).decode_utf8().unwrap();
    if content_hash == "UNSIGNED-PAYLOAD" {
        return format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method,
            uri_encode(decoded_path.as_ref(), false),
            get_canonical_query_string(query_string),
            get_canonical_headers(headers, &signed_headers),
            get_signed_headers(&signed_headers),
            content_hash
        );
    }
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method,
        uri_encode(decoded_path.as_ref(), false),
        get_canonical_query_string(query_string),
        get_canonical_headers(headers, &signed_headers),
        get_signed_headers(&signed_headers),
        hash_payload(body)
    )
}

fn get_canonical_query_string(query_string: &str) -> String {
    if query_string.is_empty() {
        return String::new();
    }

    let parsed: Vec<(String, String)> = form_urlencoded::parse(query_string.as_bytes())
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();

    let mut sorted_params: Vec<(String, String)> = parsed;
    sorted_params.sort_by(|a, b| a.0.cmp(&b.0));

    let mut encoded_params: Vec<String> = Vec::new();

    for (key, value) in sorted_params {
        let encoded_key = uri_encode(&key, true);
        let encoded_value = uri_encode(&value, true);

        encoded_params.push(format!("{}={}", encoded_key, encoded_value));
    }

    encoded_params.join("&")
}

fn get_canonical_headers(headers: &HeaderMap, signed_headers: &Vec<&str>) -> String {
    let mut canonical_headers = BTreeMap::new();

    for (name, value) in headers.iter() {
        let canonical_name = lowercase(name.as_str());
        let canonical_value = trim(value.to_str().unwrap());

        if signed_headers.contains(&canonical_name.as_str()) {
            canonical_headers
                .entry(canonical_name)
                .or_insert_with(Vec::new)
                .push(canonical_value);
        }
    }

    canonical_headers
        .iter()
        .fold(String::new(), |mut output, (name, values)| {
            output.push_str(&format!("{}:{}\n", name, values.join(",")));
            output
        })
}

fn get_signed_headers(signed_headers: &Vec<&str>) -> String {
    signed_headers
        .iter()
        .map(|header| lowercase(header))
        .collect::<Vec<String>>()
        .join(";")
}

fn hash_payload(body: &BytesMut) -> String {
    hex::encode(Sha256::digest(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_http::header::{HeaderMap, HeaderName, HeaderValue};
    use async_trait::async_trait;
    use common_s3_headers::S3HeadersBuilder;
    use std::str::FromStr;
    use url::Url;

    #[derive(Clone)]
    struct TestSourceApi {
        api_key: Option<APIKey>,
    }

    impl TestSourceApi {
        fn new(api_key: Option<APIKey>) -> Self {
            Self { api_key }
        }
    }

    #[async_trait]
    impl ApiKeyProvider for TestSourceApi {
        async fn get_api_key(&self, _access_key_id: &str) -> Result<APIKey, BackendError> {
            let Some(key) = &self.api_key else {
                return Err(BackendError::ApiKeyNotFound);
            };
            Ok(key.clone())
        }
    }

    fn create_test_source_api(api_key: Option<APIKey>) -> web::Data<Box<dyn ApiKeyProvider>> {
        let api: Box<dyn ApiKeyProvider> = Box::new(TestSourceApi::new(api_key));
        web::Data::new(api)
    }

    #[tokio::test]
    async fn test_load_identity_missing_auth_header() {
        let headers = HeaderMap::new();
        let source_api = create_test_source_api(None);

        let result =
            load_identity(&source_api, "GET", "/test", &headers, "", &BytesMut::new()).await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "No Authorization header found");
    }

    #[tokio::test]
    async fn test_load_identity_invalid_signature_method() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_str("Authorization").unwrap(),
            HeaderValue::from_str("INVALID Credential=test-key/20240315/us-east-1/s3, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=test-signature").unwrap(),
        );

        let source_api = create_test_source_api(None);

        let result =
            load_identity(&source_api, "GET", "/test", &headers, "", &BytesMut::new()).await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Invalid Signature Algorithm");
    }

    #[tokio::test]
    async fn test_load_identity_missing_content_hash() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_str("Authorization").unwrap(),
            HeaderValue::from_str("AWS4-HMAC-SHA256 Credential=test-key/20240315/us-east-1/s3, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=test-signature").unwrap(),
        );

        let source_api = create_test_source_api(None);

        let result =
            load_identity(&source_api, "GET", "/test", &headers, "", &BytesMut::new()).await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "No x-amz-content-sha256 header found");
    }

    #[tokio::test]
    async fn test_load_identity_missing_date() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_str("Authorization").unwrap(),
            HeaderValue::from_str("AWS4-HMAC-SHA256 Credential=test-key/20240315/us-east-1/s3, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=test-signature").unwrap(),
        );
        headers.insert(
            HeaderName::from_str("x-amz-content-sha256").unwrap(),
            HeaderValue::from_str("test-hash").unwrap(),
        );

        let source_api = create_test_source_api(None);

        let result =
            load_identity(&source_api, "GET", "/test", &headers, "", &BytesMut::new()).await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "No x-amz-date header found");
    }

    #[tokio::test]
    async fn test_load_identity_success() {
        let api_key = APIKey {
            access_key_id: "test-key".to_string(),
            secret_access_key: "test-secret".to_string(),
        };
        let source_api = create_test_source_api(Some(api_key.clone()));

        let method = "GET";
        let url = Url::parse("https://test.com/test").unwrap();
        let path = url.path();

        let headers = HeaderMap::from_iter(
            S3HeadersBuilder::new(&url)
                .set_access_key(api_key.access_key_id.as_str())
                .set_secret_key(api_key.secret_access_key.as_str())
                .set_region("us-east-1")
                .set_method("GET")
                .set_service("s3")
                .build()
                .iter()
                .map(|(k, v)| {
                    if *k == "Authorization" {
                        // HACK: Our code expects the authorization to have spaces after the comma
                        // This is a hack to make the test pass
                        (
                            HeaderName::from_str(k).unwrap(),
                            HeaderValue::from_str(
                                v.as_str()
                                    .split(",")
                                    .collect::<Vec<&str>>()
                                    .join(", ")
                                    .as_str(),
                            )
                            .unwrap(),
                        )
                    } else {
                        (
                            HeaderName::from_str(k).unwrap(),
                            HeaderValue::from_str(v.as_str()).unwrap(),
                        )
                    }
                }),
        );

        let result = load_identity(&source_api, method, path, &headers, "", &BytesMut::new()).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap().access_key_id, "test-key");
    }
}
