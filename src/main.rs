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

use apis::source::{RepositoryPermission, SourceAPI};
use apis::API;
use backends::common::{CommonPrefix, CompleteMultipartUpload, ListBucketResult};
use bytes::Bytes;
use core::num::NonZeroU32;

use futures_util::StreamExt;

use quick_xml::se::to_string_with_root;
use serde::Deserialize;
use serde_xml_rs::from_str;
use std::env;
use std::pin::Pin;
use std::str::from_utf8;
use std::task::{Context, Poll};
use utils::auth::{LoadIdentity, UserIdentity};

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

// TODO: Map the APIErrors to HTTP Responses

#[get("/{account_id}/{repository_id}/{key:.*}")]
async fn get_object(
    api_client: web::Data<SourceAPI>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    user_identity: web::ReqData<UserIdentity>,
) -> impl Responder {
    let (account_id, repository_id, key) = path.into_inner();
    let headers = req.headers();
    let mut range = None;
    let mut range_start = 0;
    let mut is_range_request = false;

    if let Some(range_header) = headers.get(RANGE) {
        if let Ok(r) = range_header.to_str() {
            if let Some(bytes_range) = r.strip_prefix("bytes=") {
                if let Some((start, end)) = bytes_range.split_once('-') {
                    if let Ok(s) = start.parse::<u64>() {
                        range_start = s;
                        if end.is_empty() || end.parse::<u64>().is_ok() {
                            range = Some(r.to_string());
                            is_range_request = true;
                        }
                    }
                }
            }
        }
    }

    if let Ok(client) = api_client
        .get_backend_client(&account_id, &repository_id)
        .await
    {
        match api_client
            .is_authorized(
                user_identity.into_inner(),
                &account_id,
                &repository_id,
                RepositoryPermission::Read,
            )
            .await
        {
            Ok(authorized) => {
                if !authorized {
                    return HttpResponse::Unauthorized().finish();
                }
            }
            Err(_) => return HttpResponse::InternalServerError().finish(),
        }

        // Found the repository, now try to get the object
        match client.get_object(key.clone(), range).await {
            Ok(res) => {
                let mut content_length = String::from("*");

                // Remove this if statement to increase performance since it's making an extra request just to get the total content-length
                // This is only needed for range requests and in theory, you can return a * in the Content-Range header to indicate that the content length is unknown
                if is_range_request {
                    match client.head_object(key.clone()).await {
                        Ok(head_res) => {
                            content_length = head_res.content_length.to_string();
                        }
                        Err(_) => {}
                    }
                }

                let stream = res.body.map(|result| {
                    result
                        .map(web::Bytes::from)
                        .map_err(|e| ErrorInternalServerError(e.to_string()))
                });

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

                return response.body(streaming_response);
            }
            Err(_) => HttpResponse::NotFound().finish(),
        }
    } else {
        // Could not find the repository
        return HttpResponse::NotFound().finish();
    }
}

#[derive(Debug, Deserialize)]
struct DeleteParams {
    #[serde(rename = "uploadId")]
    upload_id: Option<String>,
}

#[delete("/{account_id}/{repository_id}/{key:.*}")]
async fn delete_object(
    api_client: web::Data<SourceAPI>,
    params: web::Query<DeleteParams>,
    path: web::Path<(String, String, String)>,
    user_identity: web::ReqData<UserIdentity>,
) -> impl Responder {
    let (account_id, repository_id, key) = path.into_inner();

    if let Ok(client) = api_client
        .get_backend_client(&account_id, &repository_id)
        .await
    {
        match api_client
            .is_authorized(
                user_identity.into_inner(),
                &account_id,
                &repository_id,
                RepositoryPermission::Write,
            )
            .await
        {
            Ok(authorized) => {
                if !authorized {
                    return HttpResponse::Unauthorized().finish();
                }
            }
            Err(_) => return HttpResponse::InternalServerError().finish(),
        }

        if params.upload_id.is_none() {
            // Found the repository, now try to delete the object
            match client.delete_object(key.clone()).await {
                Ok(_) => {
                    return HttpResponse::NoContent().finish();
                }
                Err(_) => HttpResponse::NotFound().finish(),
            }
        } else {
            match client
                .abort_multipart_upload(key.clone(), params.upload_id.clone().unwrap())
                .await
            {
                Ok(_) => {
                    return HttpResponse::NoContent().finish();
                }
                Err(_) => HttpResponse::NotFound().finish(),
            }
        }
    } else {
        // Could not find the repository
        return HttpResponse::NotFound().finish();
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
    api_client: web::Data<SourceAPI>,
    req: HttpRequest,
    bytes: Bytes,
    params: web::Query<PutParams>,
    path: web::Path<(String, String, String)>,
    user_identity: web::ReqData<UserIdentity>,
) -> impl Responder {
    let (account_id, repository_id, key) = path.into_inner();
    let headers = req.headers();

    if let Ok(client) = api_client
        .get_backend_client(&account_id, &repository_id)
        .await
    {
        match api_client
            .is_authorized(
                user_identity.into_inner(),
                &account_id,
                &repository_id,
                RepositoryPermission::Write,
            )
            .await
        {
            Ok(authorized) => {
                if !authorized {
                    return HttpResponse::Unauthorized().finish();
                }
            }
            Err(_) => return HttpResponse::InternalServerError().finish(),
        }

        if params.part_number.is_none() && params.upload_id.is_none() {
            // Found the repository, now try to upload the object
            match client
                .put_object(
                    key.clone(),
                    bytes,
                    headers
                        .get(CONTENT_TYPE)
                        .and_then(|h| h.to_str().ok())
                        .map(|s| s.to_string()),
                )
                .await
            {
                Ok(_) => HttpResponse::NoContent().finish(),

                Err(_) => HttpResponse::NotFound().finish(),
            }
        } else if params.part_number.is_some() && params.upload_id.is_some() {
            match client
                .upload_multipart_part(
                    key.clone(),
                    params.upload_id.clone().unwrap(),
                    params.part_number.clone().unwrap(),
                    bytes,
                )
                .await
            {
                Ok(res) => HttpResponse::Ok()
                    .insert_header(("ETag", res.etag))
                    .finish(),

                Err(_) => HttpResponse::NotFound().finish(),
            }
        } else {
            return HttpResponse::NotFound().finish();
        }
    } else {
        // Could not find the repository
        return HttpResponse::NotFound().finish();
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
    api_client: web::Data<SourceAPI>,
    req: HttpRequest,
    params: web::Query<PostParams>,
    mut payload: web::Payload,
    path: web::Path<(String, String, String)>,
    user_identity: web::ReqData<UserIdentity>,
) -> impl Responder {
    let (account_id, repository_id, key) = path.into_inner();
    let headers = req.headers();

    if let Ok(client) = api_client
        .get_backend_client(&account_id, &repository_id)
        .await
    {
        match api_client
            .is_authorized(
                user_identity.into_inner(),
                &account_id,
                &repository_id,
                RepositoryPermission::Write,
            )
            .await
        {
            Ok(authorized) => {
                if !authorized {
                    return HttpResponse::Unauthorized().finish();
                }
            }
            Err(_) => return HttpResponse::InternalServerError().finish(),
        }

        if params.uploads.is_some() {
            match client
                .create_multipart_upload(
                    key,
                    headers
                        .get(CONTENT_TYPE)
                        .and_then(|h| h.to_str().ok())
                        .map(|s| s.to_string()),
                )
                .await
            {
                Ok(res) => match to_string_with_root("InitiateMultipartUploadResult", &res) {
                    Ok(serialized) => {
                        return HttpResponse::Ok()
                            .content_type("application/xml")
                            .body(serialized)
                    }
                    Err(_) => return HttpResponse::InternalServerError().finish(),
                },
                Err(_) => {
                    return HttpResponse::NotFound().finish();
                }
            }
        } else if params.upload_id.is_some() {
            let mut body = String::new();
            while let Some(chunk) = payload.next().await {
                match chunk {
                    Ok(chunk) => match from_utf8(&chunk) {
                        Ok(s) => body.push_str(s),
                        Err(_) => return HttpResponse::BadRequest().body("Invalid UTF-8"),
                    },
                    Err(_) => return HttpResponse::InternalServerError().finish(),
                }
            }

            match from_str::<CompleteMultipartUpload>(&body) {
                Ok(upload) => {
                    match client
                        .complete_multipart_upload(
                            key,
                            params.upload_id.clone().unwrap(),
                            upload.parts,
                        )
                        .await
                    {
                        Ok(res) => match to_string_with_root("CompleteMultipartUploadResult", &res)
                        {
                            Ok(serialized) => {
                                return HttpResponse::Ok()
                                    .content_type("application/xml")
                                    .body(serialized)
                            }
                            Err(_) => return HttpResponse::InternalServerError().finish(),
                        },
                        Err(_) => {
                            return HttpResponse::NotFound().finish();
                        }
                    }
                }
                Err(_) => {
                    return HttpResponse::BadRequest().finish();
                }
            }
        } else {
            return HttpResponse::NotFound().finish();
        }
    } else {
        // Could not find the repository
        return HttpResponse::NotFound().finish();
    }
}

#[head("/{account_id}/{repository_id}/{key:.*}")]
async fn head_object(
    api_client: web::Data<SourceAPI>,
    path: web::Path<(String, String, String)>,
    user_identity: web::ReqData<UserIdentity>,
) -> impl Responder {
    let (account_id, repository_id, key) = path.into_inner();

    match api_client
        .get_backend_client(&account_id, &repository_id)
        .await
    {
        Ok(client) => {
            match api_client
                .is_authorized(
                    user_identity.into_inner(),
                    &account_id,
                    &repository_id,
                    RepositoryPermission::Read,
                )
                .await
            {
                Ok(authorized) => {
                    if !authorized {
                        return HttpResponse::Unauthorized().finish();
                    }
                }
                Err(_) => return HttpResponse::InternalServerError().finish(),
            }

            match client.head_object(key.clone()).await {
                Ok(res) => HttpResponse::Ok()
                    .insert_header(("Content-Type", res.content_type))
                    .insert_header(("Last-Modified", res.last_modified))
                    .insert_header(("ETag", res.etag))
                    .body(BoxBody::new(FakeBody {
                        size: res.content_length as usize,
                    })),
                Err(error) => error.to_response(),
            }
        }
        Err(_) => HttpResponse::NotFound().finish(),
    }
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
    api_client: web::Data<SourceAPI>,
    info: web::Query<ListObjectsV2Query>,
    path: web::Path<String>,
    user_identity: web::ReqData<UserIdentity>,
) -> impl Responder {
    let account_id = path.into_inner();

    if info.prefix.clone().is_some_and(|s| s.is_empty()) || info.prefix.is_none() {
        match api_client
            .get_account(account_id.clone(), (*user_identity).clone())
            .await
        {
            Ok(account) => {
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

                match to_string_with_root("ListBucketResult", &list_response) {
                    Ok(serialized) => {
                        return HttpResponse::Ok()
                            .content_type("application/xml")
                            .body(serialized)
                    }
                    Err(_) => return HttpResponse::InternalServerError().finish(),
                }
            }
            Err(_) => return HttpResponse::InternalServerError().finish(),
        }
    }

    let path_prefix = info.prefix.clone().unwrap_or("".to_string());

    let (repository_id, prefix) = split_at_first_slash(&path_prefix);

    let mut max_keys = NonZeroU32::new(1000).unwrap();
    if let Some(mk) = info.max_keys {
        max_keys = mk;
    }

    if let Ok(client) = api_client
        .get_backend_client(&account_id, &repository_id.to_string())
        .await
    {
        match api_client
            .is_authorized(
                user_identity.into_inner(),
                &account_id,
                &repository_id.to_string(),
                RepositoryPermission::Read,
            )
            .await
        {
            Ok(authorized) => {
                if !authorized {
                    return HttpResponse::Unauthorized().finish();
                }
            }
            Err(_) => return HttpResponse::InternalServerError().finish(),
        }

        // We're listing within a repository, so we need to query the object store backend
        match client
            .list_objects_v2(
                prefix.to_string(),
                info.continuation_token.clone(),
                info.delimiter.clone(),
                max_keys,
            )
            .await
        {
            Ok(res) => match to_string_with_root("ListBucketResult", &res) {
                Ok(serialized) => HttpResponse::Ok()
                    .content_type("application/xml")
                    .body(serialized),
                Err(e) => HttpResponse::InternalServerError().finish(),
            },
            Err(_) => HttpResponse::NotFound().finish(),
        }
        // Found the repository, now make the list objects request
    } else {
        // Could not find the repository
        return HttpResponse::NotFound().finish();
    }
}

#[get("/list_accounts")]
async fn list_accounts(api_client: web::Data<SourceAPI>) -> impl Responder {
    // TODO: Change to some existing default accId & repoId
    let account_id = String::from("adarsh");
    let repository_id = String::from("adarsh-dev");

    if let Ok(client) = api_client
        .get_backend_client(&account_id, &repository_id.to_string())
        .await
    {
        match client
            // Pass default static values
            .list_buckets_accounts(
                "".to_string(),
                None,
                Some("/".to_string()),
                NonZeroU32::new(1000).unwrap(),
            )
            .await
        {
            Ok(res) => match to_string_with_root("ListBucketResult", &res) {
                Ok(serialized) => HttpResponse::Ok()
                    .content_type("application/xml")
                    .body(serialized),
                Err(e) => HttpResponse::InternalServerError().finish(),
            },
            Err(_) => HttpResponse::NotFound().finish(),
        }
    } else {
        // Could not find the repository
        return HttpResponse::NotFound().finish();
    }
}

#[get("/")]
async fn index() -> impl Responder {
    HttpResponse::Ok().body(format!("Source Cooperative Data Proxy v{}", VERSION))
}

// Main function to set up and run the HTTP server
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let source_api_url = env::var("SOURCE_API_URL").unwrap();
    let source_api = web::Data::new(SourceAPI::new(source_api_url));
    json_env_logger::builder()
        .target(json_env_logger::env_logger::Target::Stdout)
        .init();
    // env_logger::init_from_env(Env::default().default_filter_or("info"));

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
            .wrap(utils::apache_logger::ApacheLogger)
            .wrap(LoadIdentity)
            // Register the endpoints
            .service(get_object)
            .service(delete_object)
            .service(post_handler)
            .service(put_object)
            .service(head_object)
            .service(list_accounts)
            .service(list_objects)
            .service(index)
    })
    .bind("0.0.0.0:8080")?
    .run()
    .await
}
