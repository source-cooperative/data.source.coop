use crate::apis::source::SourceAPI;
use crate::apis::API;
use crate::utils::auth::load_identity;
use crate::utils::core::get_query_params;
use crate::{apis::source::APIKey, backends::common::Repository};
use actix_web::body::EitherBody;
use actix_web::dev::ServiceResponse;
use actix_web::web;
use actix_web::{
    dev::{self, Service, ServiceRequest, Transform},
    web::BytesMut,
    Error, HttpMessage,
};
use futures_util::future::{ok, Ready};
use futures_util::{future::LocalBoxFuture, stream::StreamExt};
use std::{collections::HashMap, rc::Rc, sync::Arc};

#[derive(Clone, Debug)]
pub struct RequestContext {
    pub account_id: Option<String>,
    pub repository_id: Option<String>,
    pub key: Option<String>,
    pub identity: Option<APIKey>,
    pub is_virtual_object: bool,
    pub client: Option<Arc<dyn Repository>>,
    pub body: BytesMut,
}

pub struct LoadContext;

impl<S: 'static, B> Transform<S, ServiceRequest> for LoadContext
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = LoadContextMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ok(LoadContextMiddleware {
            service: Rc::new(service),
        })
    }
}

pub struct LoadContextMiddleware<S> {
    service: Rc<S>,
}

impl<S, B> Service<ServiceRequest> for LoadContextMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    dev::forward_ready!(service);

    fn call(&self, mut req: ServiceRequest) -> Self::Future {
        let svc = self.service.clone();

        Box::pin(async move {
            let mut ctx = RequestContext {
                account_id: None,
                repository_id: None,
                key: None,
                is_virtual_object: false,
                identity: None,
                client: None,
                body: BytesMut::new(),
            };

            // Extract necessary information before taking the payload
            let path = req.path().to_owned();
            let query_string = req.query_string().to_owned();
            let method = req.method().to_string();
            let headers = req.headers().clone();

            // Load the request body
            let mut body = BytesMut::new();
            let mut payload = req.take_payload();
            while let Some(chunk) = payload.next().await {
                body.extend_from_slice(&chunk?);
            }
            ctx.body = body;

            // Now it's safe to borrow req immutably
            let source_api: &web::Data<SourceAPI> = req.app_data::<web::Data<SourceAPI>>().unwrap();

            // Load the request query parameters
            let params = get_query_params(&query_string);

            // Load the request account_id, repository_id, and key from the path
            let (account_id, repository_id, key) = extract_request_parts(&path, &params);
            ctx.account_id = account_id;
            ctx.repository_id = repository_id;
            ctx.key = key;

            // Check if the object in question is a virtual object
            if let Some(key) = &ctx.key {
                if key.starts_with(".source/") {
                    ctx.is_virtual_object = true;
                }
            }

            // Load the identity from the request
            match load_identity(
                source_api,
                &method,
                &path,
                &headers,
                &query_string,
                &ctx.body,
            )
            .await
            {
                Ok(identity) => {
                    ctx.identity = identity;
                }
                Err(e) => {
                    return Ok(req.into_response(e.to_response()).map_into_right_body());
                }
            }

            match source_api.get_backend_client(&ctx).await {
                Ok(client) => {
                    ctx.client = Some(client);
                }
                Err(e) => {
                    return Ok(req.into_response(e.to_response()).map_into_right_body());
                }
            }

            // Insert the context into the request extensions
            req.extensions_mut().insert(ctx);

            let res = svc.call(req).await?;

            Ok(res.map_into_left_body())
        })
    }
}

/// Extracts account_id, repository_id, and key from the given path and query parameters.
///
/// # Arguments
///
/// - `path` - A string slice that holds the request path.
/// - `params` - A reference to a HashMap containing query parameters.
///
/// # Returns
///
/// A tuple of three `Option<String>` values:
/// - The first element is the account_id.
/// - The second element is the repository_id.
/// - The third element is the key.
///
/// # Details
///
/// - Extracts account_id and repository_id from the path segments.
/// - Extracts the key from the remaining path segments.
/// - If a 'prefix' query parameter is present, it overrides the repository_id and key.
/// - Returns None for any component that is not present or empty.
fn extract_request_parts(
    path: &str,
    params: &HashMap<String, String>,
) -> (Option<String>, Option<String>, Option<String>) {
    // Load the request account_id, repository_id, and key from the path
    let split_path: Vec<&str> = path.split('/').collect();
    let mut parts = split_path.into_iter().skip(1);
    let account_id = parts.next().map(|s| s.to_string());
    let mut repository_id = parts.next().map(|s| s.to_string());
    let mut key = parts.collect::<Vec<&str>>().join("/");

    if key.is_empty() {
        key = String::new();
    }

    // Load the repository_id and key from the prefix query parameter, if present
    if let Some(prefix) = params.get("prefix") {
        let split_prefix: Vec<&str> = prefix.split('/').collect();
        repository_id = Some(split_prefix[0].to_string());
        key = split_prefix
            .into_iter()
            .skip(1)
            .collect::<Vec<&str>>()
            .join("/");
    }

    (
        account_id,
        repository_id,
        if key.is_empty() { None } else { Some(key) },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_request_parts() {
        let params = HashMap::new();

        // Test with standard path
        let (account_id, repository_id, key) =
            extract_request_parts("/account1/repo1/key/path", &params);
        assert_eq!(account_id, Some("account1".to_string()));
        assert_eq!(repository_id, Some("repo1".to_string()));
        assert_eq!(key, Some("key/path".to_string()));

        // Test with no key
        let (account_id, repository_id, key) = extract_request_parts("/account2/repo2", &params);
        assert_eq!(account_id, Some("account2".to_string()));
        assert_eq!(repository_id, Some("repo2".to_string()));
        assert_eq!(key, None);

        // Test with prefix parameter
        let mut params_with_prefix = HashMap::new();
        params_with_prefix.insert("prefix".to_string(), "repo3/prefix/key".to_string());
        let (account_id, repository_id, key) =
            extract_request_parts("/account3", &params_with_prefix);
        assert_eq!(account_id, Some("account3".to_string()));
        assert_eq!(repository_id, Some("repo3".to_string()));
        assert_eq!(key, Some("prefix/key".to_string()));

        // Test with empty path
        let (account_id, repository_id, key) = extract_request_parts("", &params);
        assert_eq!(account_id, None);
        assert_eq!(repository_id, None);
        assert_eq!(key, None);
    }
}
