mod clients;
mod utils;

use crate::clients::common::fetch_repository_client;
use crate::utils::core::{FakeBody, StreamingResponse};
use actix_web::error::ErrorInternalServerError;
use actix_web::{
    get, head, http::header::RANGE, web, App, HttpRequest, HttpResponse, HttpServer, Responder,
};
use core::num::NonZeroU32;
use futures_util::StreamExt;
use quick_xml::se::to_string_with_root;
use serde::Deserialize;

// TODO: Handdle errors better

fn split_at_first_slash(input: String) -> (String, String) {
    match input.find('/') {
        Some(index) => {
            let (before, after) = input.split_at(index);
            (before.to_string(), after[1..].to_string())
        }
        None => (input, String::new()),
    }
}

#[get("/{account_id}/{repository_id}/{key:.*}")]
async fn get_object(req: HttpRequest, path: web::Path<(String, String, String)>) -> impl Responder {
    let (account_id, repository_id, key) = path.into_inner();
    let headers = req.headers();
    let mut range = Some("".to_string());

    if let Some(range_header) = headers.get(RANGE) {
        if let Ok(r) = range_header.to_str() {
            range = Some(r.to_string());
        }
    }

    match fetch_repository_client(&account_id, &repository_id).await {
        Ok(repository_client) => match repository_client.get_object(key.clone(), range).await {
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
        },
        Err(_) => HttpResponse::NotFound().finish(),
    }
}

#[head("/{account_id}/{repository_id}/{key:.*}")]
async fn head_object(path: web::Path<(String, String, String)>) -> impl Responder {
    let (account_id, repository_id, key) = path.into_inner();

    match fetch_repository_client(&account_id, &repository_id).await {
        Ok(repository_client) => match repository_client.head_object(key.clone()).await {
            Ok(res) => HttpResponse::Ok()
                .insert_header(("Content-Type", res.content_type))
                .insert_header(("Last-Modified", res.last_modified))
                .insert_header(("ETag", res.etag))
                .body(FakeBody {
                    size: res.content_length as usize,
                }),
            Err(_) => HttpResponse::NotFound().finish(),
        },
        Err(_) => HttpResponse::NotFound().finish(),
    }
}

#[derive(Deserialize)]
struct ListObjectsV2Query {
    #[serde(rename = "prefix")]
    prefix: String,
    #[serde(rename = "list-type")]
    _list_type: u8,
    #[serde(rename = "max-keys")]
    max_keys: Option<NonZeroU32>,
    #[serde(rename = "continuation-token")]
    continuation_token: Option<String>,
}

#[get("/{account_id}")]
async fn list_objects(
    info: web::Query<ListObjectsV2Query>,
    path: web::Path<String>,
) -> impl Responder {
    let account_id = path.into_inner();

    let (repository_id, prefix) = split_at_first_slash(info.prefix.clone());

    let mut max_keys = NonZeroU32::new(20).unwrap_or(NonZeroU32::new(20).unwrap());
    if let Some(mk) = info.max_keys {
        max_keys = mk;
    }

    match fetch_repository_client(&account_id, &repository_id).await {
        Ok(repository_client) => match repository_client
            .list_objects_v2(prefix.clone(), info.continuation_token.clone(), max_keys)
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
            Err(_) => HttpResponse::NotFound().finish(),
        },
        Err(_) => HttpResponse::NotFound().finish(),
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    HttpServer::new(|| {
        App::new()
            .service(get_object)
            .service(head_object)
            .service(list_objects)
    })
    .bind("0.0.0.0:8080")?
    .run()
    .await
}
