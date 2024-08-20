mod apis;
mod backends;
mod utils;
use crate::utils::core::{split_at_first_slash, StreamingResponse};
use actix_cors::Cors;
use actix_web::error::ErrorInternalServerError;
use actix_web::{
    get, head, http::header::RANGE, middleware, web, App, HttpRequest, HttpResponse, HttpServer,
    Responder,
};
use apis::source::SourceAPI;
use apis::API;
use backends::common::{CommonPrefix, ListBucketResult};
use core::num::NonZeroU32;
use env_logger::Env;
use futures_util::StreamExt;
use quick_xml::se::to_string_with_root;
use serde::Deserialize;

const VERSION: &str = env!("CARGO_PKG_VERSION");

// TODO: Map the APIErrors to HTTP Responses

#[get("/{account_id}/{repository_id}/{key:.*}")]
async fn get_object(
    api_client: web::Data<SourceAPI>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
) -> impl Responder {
    let (account_id, repository_id, key) = path.into_inner();
    let headers = req.headers();
    let mut range = Some("".to_string());

    if let Some(range_header) = headers.get(RANGE) {
        if let Ok(r) = range_header.to_str() {
            range = Some(r.to_string());
        }
    }

    if let Ok(client) = api_client
        .get_backend_client(account_id, repository_id)
        .await
    {
        // Found the repository, now try to get the object
        match client.get_object(key.clone(), range).await {
            Ok(res) => {
                let stream = res.body.map(|result| {
                    result
                        .map(web::Bytes::from)
                        .map_err(|e| ErrorInternalServerError(e.to_string()))
                });

                let streaming_response = StreamingResponse::new(stream, res.content_length);

                return HttpResponse::Ok()
                    .insert_header(("Content-Type", res.content_type))
                    .insert_header(("Last-Modified", res.last_modified))
                    .insert_header(("Content-Length", res.content_length.to_string()))
                    .insert_header(("ETag", res.etag))
                    .body(streaming_response);
            }
            Err(_) => HttpResponse::NotFound().finish(),
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
) -> impl Responder {
    let (account_id, repository_id, key) = path.into_inner();

    match api_client
        .get_backend_client(account_id, repository_id)
        .await
    {
        Ok(client) => match client.head_object(key.clone()).await {
            Ok(res) => HttpResponse::Ok()
                .insert_header(("Content-Type", res.content_type))
                .insert_header(("Last-Modified", res.last_modified))
                .insert_header(("ETag", res.etag))
                .insert_header(("Content-Length", res.content_length.to_string()))
                .finish(),
            Err(error) => error.to_response(),
        },
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
) -> impl Responder {
    let account_id = path.into_inner();

    if info.prefix.is_none() {
        match api_client.get_account(account_id.clone()).await {
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

    let mut max_keys = NonZeroU32::new(20).unwrap_or(NonZeroU32::new(20).unwrap());
    if let Some(mk) = info.max_keys {
        max_keys = mk;
    }

    if let Ok(client) = api_client
        .get_backend_client(account_id.clone(), repository_id.to_string())
        .await
    {
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
                Err(e) => {
                    eprintln!("Serialization error: {:?}", e);
                    HttpResponse::InternalServerError().finish()
                }
            },
            Err(_) => {
                println!("Could Not List Objects");
                HttpResponse::NotFound().finish()
            }
        }
        // Found the repository, now make the list objects request
    } else {
        println!("Could Not Find Repository");
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
    let source_api = web::Data::new(SourceAPI::new("https://api.source.coop".to_string()));
    env_logger::init_from_env(Env::default().default_filter_or("info"));

    HttpServer::new(move || {
        App::new()
            .app_data(source_api.clone())
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
            // Register the endpoints
            .service(get_object)
            .service(head_object)
            .service(list_objects)
            .service(index)
    })
    .bind("0.0.0.0:8080")?
    .run()
    .await
}
