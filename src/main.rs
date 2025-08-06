mod apis;
mod backends;
mod utils;
use crate::utils::core::{split_at_first_slash, StreamingResponse};
use actix_cors::Cors;
use actix_web::body::{BodySize, BoxBody, MessageBody};
use actix_web::error::ErrorInternalServerError;
use actix_web::{
    delete, get, head, http::header::CONTENT_TYPE, http::header::RANGE, middleware, post, put, web,
    App, HttpRequest, HttpResponse, HttpServer, Responder,
};

use apis::source::{RepositoryPermission, SourceApi};
use apis::Api;
use backends::common::{CommonPrefix, CompleteMultipartUpload, ListBucketResult};
use bytes::Bytes;
use core::num::NonZeroU32;
use env_logger::Env;
use futures_util::StreamExt;
use quick_xml::se::to_string_with_root;
use serde::Deserialize;
use serde_xml_rs::from_str;
use std::env;
use std::fmt::Debug;
use std::pin::Pin;
use std::str::from_utf8;
use std::task::{Context, Poll};
use utils::auth::{LoadIdentity, UserIdentity};
use utils::errors::BackendError;
const VERSION: &str = env!("CARGO_PKG_VERSION");

struct FakeBody {
    size: usize,
}

impl MessageBody for FakeBody {
    type Error = actix_web::Error;

    fn size(&self) -> BodySize {
        BodySize::Sized(self.size as u64)
    }

    fn poll_next(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Bytes, Self::Error>>> {
        Poll::Ready(None)
    }
}

#[get("/{account_id}/{repository_id}/{key:.*}")]
async fn get_object(
    api_client: web::Data<SourceApi>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    user_identity: web::ReqData<UserIdentity>,
) -> Result<impl Responder, BackendError> {
    let (account_id, repository_id, key) = path.into_inner();
    let headers = req.headers();
    let mut range_start = 0;
    let mut is_range_request = false;

    let range = headers
        .get(RANGE)
        .and_then(|h| h.to_str().ok())
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|bytes_range| bytes_range.split_once('-'))
        .and_then(|(start, end)| {
            start.parse::<u64>().ok().map(|s| {
                range_start = s;
                if end.is_empty() || end.parse::<u64>().is_ok() {
                    is_range_request = true;
                    Some(format!("bytes={start}-{end}"))
                } else {
                    None
                }
            })
        })
        .flatten();

    let client = api_client
        .get_backend_client(&account_id, &repository_id)
        .await?;

    api_client
        .assert_authorized(
            user_identity.into_inner(),
            &account_id,
            &repository_id,
            RepositoryPermission::Read,
        )
        .await?;

    // Found the repository, now try to get the object
    let res = client.get_object(key.clone(), range).await?;

    let mut content_length = String::from("*");
    // Remove this if statement to increase performance since it's making an extra request just to get the total content-length
    // This is only needed for range requests and in theory, you can return a * in the Content-Range header to indicate that the content length is unknown
    if is_range_request {
        content_length = client
            .head_object(key.clone())
            .await?
            .content_length
            .to_string();
    }

    let stream = res
        .body
        .map(|result| result.map_err(|e| ErrorInternalServerError(e.to_string())));

    let streaming_response = StreamingResponse::new(stream, res.content_length);
    let mut response = if is_range_request {
        HttpResponse::PartialContent()
    } else {
        HttpResponse::Ok()
    };

    let mut response = response
        .insert_header(("Content-Type", res.content_type))
        .insert_header(("Last-Modified", res.last_modified))
        .insert_header(("Content-Length", res.content_length.to_string()))
        .insert_header(("ETag", res.etag));

    if is_range_request {
        response = response.insert_header((
            "Content-Range",
            format!(
                "bytes {}-{}/{}",
                range_start,
                range_start + res.content_length - 1,
                content_length
            ),
        ));
    }

    Ok(response.body(streaming_response))
}

#[derive(Debug, Deserialize)]
struct DeleteParams {
    #[serde(rename = "uploadId")]
    upload_id: Option<String>,
}

#[delete("/{account_id}/{repository_id}/{key:.*}")]
async fn delete_object(
    api_client: web::Data<SourceApi>,
    params: web::Query<DeleteParams>,
    path: web::Path<(String, String, String)>,
    user_identity: web::ReqData<UserIdentity>,
) -> Result<impl Responder, BackendError> {
    let (account_id, repository_id, key) = path.into_inner();

    let client = api_client
        .get_backend_client(&account_id, &repository_id)
        .await?;

    api_client
        .assert_authorized(
            user_identity.into_inner(),
            &account_id,
            &repository_id,
            RepositoryPermission::Write,
        )
        .await?;

    if params.upload_id.is_none() {
        // Found the repository, now try to delete the object
        client.delete_object(key.clone()).await?;
        Ok(HttpResponse::NoContent().finish())
    } else {
        client
            .abort_multipart_upload(key.clone(), params.upload_id.clone().unwrap())
            .await?;
        Ok(HttpResponse::NoContent().finish())
    }
}

#[derive(Debug, Deserialize)]
struct PutParams {
    #[serde(rename = "partNumber")]
    part_number: Option<String>,
    #[serde(rename = "uploadId")]
    upload_id: Option<String>,
}

#[put("/{account_id}/{repository_id}/{key:.*}")]
async fn put_object(
    api_client: web::Data<SourceApi>,
    req: HttpRequest,
    bytes: Bytes,
    params: web::Query<PutParams>,
    path: web::Path<(String, String, String)>,
    user_identity: web::ReqData<UserIdentity>,
) -> Result<impl Responder, BackendError> {
    let (account_id, repository_id, key) = path.into_inner();
    let headers = req.headers();

    let client = api_client
        .get_backend_client(&account_id, &repository_id)
        .await?;

    api_client
        .assert_authorized(
            user_identity.into_inner(),
            &account_id,
            &repository_id,
            RepositoryPermission::Write,
        )
        .await?;

    if params.part_number.is_none() && params.upload_id.is_none() {
        // Check if this is a server-side copy operation
        if let Some(header_copy_identifier) = req.headers().get("x-amz-copy-source") {
            let copy_identifier_path = header_copy_identifier.to_str().unwrap_or("");
            client
                .copy_object((&copy_identifier_path).to_string(), key.clone(), None)
                .await?;
            Ok(HttpResponse::NoContent().finish())
        } else {
            // Found the repository, now try to upload the object
            client
                .put_object(
                    key.clone(),
                    bytes,
                    headers
                        .get(CONTENT_TYPE)
                        .and_then(|h| h.to_str().ok())
                        .map(|s| s.to_string()),
                )
                .await?;
            Ok(HttpResponse::NoContent().finish())
        }
    } else if params.part_number.is_some() && params.upload_id.is_some() {
        let res = client
            .upload_multipart_part(
                key.clone(),
                params.upload_id.clone().unwrap(),
                params.part_number.clone().unwrap(),
                bytes,
            )
            .await?;
        Ok(HttpResponse::Ok()
            .insert_header(("ETag", res.etag))
            .finish())
    } else {
        return Err(BackendError::InvalidRequest(
            "Must provide both part number and upload id or neither.".to_string(),
        ));
    }
}

#[derive(Debug, Deserialize)]
struct PostParams {
    uploads: Option<String>,
    #[serde(rename = "uploadId")]
    upload_id: Option<String>,
}

#[post("/{account_id}/{repository_id}/{key:.*}")]
async fn post_handler(
    api_client: web::Data<SourceApi>,
    req: HttpRequest,
    params: web::Query<PostParams>,
    mut payload: web::Payload,
    path: web::Path<(String, String, String)>,
    user_identity: web::ReqData<UserIdentity>,
) -> Result<impl Responder, BackendError> {
    let (account_id, repository_id, key) = path.into_inner();
    let headers = req.headers();

    let client = api_client
        .get_backend_client(&account_id, &repository_id)
        .await?;

    api_client
        .assert_authorized(
            user_identity.into_inner(),
            &account_id,
            &repository_id,
            RepositoryPermission::Write,
        )
        .await?;

    if params.uploads.is_some() {
        let res = client
            .create_multipart_upload(
                key,
                headers
                    .get(CONTENT_TYPE)
                    .and_then(|h| h.to_str().ok())
                    .map(|s| s.to_string()),
            )
            .await?;
        let serialized = to_string_with_root("InitiateMultipartUploadResult", &res)?;
        Ok(HttpResponse::Ok()
            .content_type("application/xml")
            .body(serialized))
    } else if params.upload_id.is_some() {
        let mut body = String::new();
        while let Some(chunk) = payload.next().await {
            match chunk {
                Ok(chunk) => match from_utf8(&chunk) {
                    Ok(s) => body.push_str(s),
                    Err(_) => {
                        return Err(BackendError::InvalidRequest("Invalid UTF-8".to_string()))
                    }
                },
                Err(err) => return Err(BackendError::UnexpectedApiError(err.to_string())),
            }
        }

        let upload = from_str::<CompleteMultipartUpload>(&body)?;
        let res = client
            .complete_multipart_upload(key, params.upload_id.clone().unwrap(), upload.parts)
            .await?;
        let serialized = to_string_with_root("CompleteMultipartUploadResult", &res)?;
        Ok(HttpResponse::Ok()
            .content_type("application/xml")
            .body(serialized))
    } else {
        return Err(BackendError::InvalidRequest(
            "Must provide either uploads or uploadId".to_string(),
        ));
    }
}

#[head("/{account_id}/{repository_id}/{key:.*}")]
async fn head_object(
    api_client: web::Data<SourceApi>,
    path: web::Path<(String, String, String)>,
    user_identity: web::ReqData<UserIdentity>,
) -> Result<impl Responder, BackendError> {
    let (account_id, repository_id, key) = path.into_inner();

    let client = api_client
        .get_backend_client(&account_id, &repository_id)
        .await?;

    api_client
        .assert_authorized(
            user_identity.into_inner(),
            &account_id,
            &repository_id,
            RepositoryPermission::Read,
        )
        .await?;

    let res = client.head_object(key.clone()).await?;
    Ok(HttpResponse::Ok()
        .insert_header(("Content-Type", res.content_type))
        .insert_header(("Last-Modified", res.last_modified))
        .insert_header(("ETag", res.etag))
        .body(BoxBody::new(FakeBody {
            size: res.content_length as usize,
        })))
}

#[derive(Deserialize)]
struct ListObjectsV2Query {
    #[serde(rename = "prefix")]
    prefix: Option<String>,
    #[serde(rename = "list-type")]
    _list_type: u8,
    #[serde(rename = "max-keys")]
    max_keys: Option<NonZeroU32>,
    #[serde(rename = "delimiter")]
    delimiter: Option<String>,
    #[serde(rename = "continuation-token")]
    continuation_token: Option<String>,
}

#[get("/{account_id}")]
async fn list_objects(
    api_client: web::Data<SourceApi>,
    info: web::Query<ListObjectsV2Query>,
    path: web::Path<String>,
    user_identity: web::ReqData<UserIdentity>,
) -> Result<impl Responder, BackendError> {
    let account_id = path.into_inner();

    if info.prefix.clone().is_some_and(|s| s.is_empty()) || info.prefix.is_none() {
        let account = api_client
            .get_account(account_id.clone(), (*user_identity).clone())
            .await?;

        let repositories = account.repositories;
        let mut common_prefixes = Vec::new();
        for repository_id in repositories.iter() {
            common_prefixes.push(CommonPrefix {
                prefix: format!("{}/", repository_id.clone()),
            });
        }
        let list_response = ListBucketResult {
            name: account_id.clone(),
            prefix: "/".to_string(),
            key_count: 0,
            max_keys: 0,
            is_truncated: false,
            contents: vec![],
            common_prefixes,
            next_continuation_token: None,
        };

        let serialized = to_string_with_root("ListBucketResult", &list_response)?;
        return Ok(HttpResponse::Ok()
            .content_type("application/xml")
            .body(serialized));
    }

    let path_prefix = info.prefix.clone().unwrap_or("".to_string());

    let (repository_id, prefix) = split_at_first_slash(&path_prefix);

    let mut max_keys = NonZeroU32::new(1000).unwrap();
    if let Some(mk) = info.max_keys {
        max_keys = mk;
    }

    let client = api_client
        .get_backend_client(&account_id, repository_id)
        .await?;

    api_client
        .assert_authorized(
            user_identity.into_inner(),
            &account_id,
            repository_id,
            RepositoryPermission::Read,
        )
        .await?;

    // We're listing within a repository, so we need to query the object store backend
    let res = client
        .list_objects_v2(
            prefix.to_string(),
            info.continuation_token.clone(),
            info.delimiter.clone(),
            max_keys,
        )
        .await?;

    let serialized = to_string_with_root("ListBucketResult", &res)?;

    Ok(HttpResponse::Ok()
        .content_type("application/xml")
        .body(serialized))
}

#[get("/")]
async fn index() -> impl Responder {
    HttpResponse::Ok().body(format!("Source Cooperative Data Proxy v{VERSION}"))
}

// Main function to set up and run the HTTP server
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let source_api_url = env::var("SOURCE_API_URL").expect("SOURCE_API_URL must be set");
    let proxy_url = env::var("SOURCE_API_PROXY_URL").ok(); // Optional proxy for the Source API
    let source_api = web::Data::new(SourceApi::new(source_api_url, proxy_url));
    env_logger::init_from_env(Env::default().default_filter_or("info"));

    HttpServer::new(move || {
        App::new()
            .app_data(web::PayloadConfig::new(1024 * 1024 * 50))
            .app_data(source_api.clone())
            .app_data(web::Data::new(UserIdentity { api_key: None }))
            .wrap(
                // Configure CORS
                Cors::default()
                    .allow_any_origin()
                    .allow_any_method()
                    .allow_any_header()
                    .supports_credentials()
                    .block_on_origin_mismatch(false)
                    .max_age(3600),
            )
            .wrap(middleware::NormalizePath::trim())
            .wrap(middleware::DefaultHeaders::new().add(("X-Version", VERSION)))
            .wrap(middleware::Logger::default())
            .wrap(LoadIdentity)
            // Register the endpoints
            .service(get_object)
            .service(delete_object)
            .service(post_handler)
            .service(put_object)
            .service(head_object)
            .service(list_objects)
            .service(index)
    })
    .bind("0.0.0.0:8080")?
    .run()
    .await
}
