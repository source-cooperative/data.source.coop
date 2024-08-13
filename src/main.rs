mod utils;
mod clients;

use actix_web::{
    http::header::RANGE,
    get, head, web, App, Error as ActixError, HttpRequest, HttpResponse, HttpServer, Responder,
};
use futures::TryStreamExt;
use rusoto_core::Region;
use rusoto_s3::{GetObjectRequest, HeadObjectRequest, S3Client, S3};
use serde::Deserialize;
use quick_xml::se::to_string_with_root;
use crate::utils::core::{S3ObjectStream, FakeBody};
use crate::clients::common::fetch_repository_client;


fn split_at_first_slash(input: String) -> (String, String) {
    match input.find('/') {
        Some(index) => {
            let (before, after) = input.split_at(index);
            (before.to_string(), after[1..].to_string())
        }
        None => (input, String::new()),
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    HttpServer::new(|| App::new().service(get_object).service(head_object).service(list_objects))
        .bind("0.0.0.0:8080")?
        .run()
        .await
}

#[get("/{account_id}/{repository_id}/{key:.*}")]
async fn get_object(req: HttpRequest, path: web::Path<(String, String, String)>) -> impl Responder {
    let (account_id, repository_id, key) = path.into_inner();
    let headers = req.headers();

    let client = S3Client::new(Region::UsWest2);

    let mut request = GetObjectRequest {
        bucket: "us-west-2.opendata.source.coop".to_string(),
        key: format!("{}/{}/{}", account_id, repository_id, key),
        ..Default::default()
    };

    if let Some(range_header) = headers.get(RANGE) {
        if let Ok(range) = range_header.to_str() {
            // Add range to the S3 request
            request.range = Some(range.to_string());
        }
    }

    match client.get_object(request).await {
        Ok(output) => {
            let content_length = output.content_length.unwrap_or(0);
            let content_type = output.content_type.unwrap_or_else(|| "application/octet-stream".to_string());
            let last_modified = output.last_modified.unwrap_or_else(|| "Thu, 1 Jan 1970 00:00:00 GMT".to_string());

            if let Some(body) = output.body {
                // Create a stream from the body
                let stream = body.map_ok(|b| web::Bytes::from(b)).map_err(ActixError::from);
                let s3_stream = S3ObjectStream::new(stream, content_length as u64);

                HttpResponse::Ok()
                    .insert_header(("Last-Modified", last_modified))
                    .content_type(content_type)
                    .body(s3_stream)
            } else {
                HttpResponse::InternalServerError().body("Failed to get object body")
            }
        }
        Err(err) => {
            dbg!(err);
            HttpResponse::NotFound().finish()
        }
    }
}

#[head("/{account_id}/{repository_id}/{key:.*}")]
async fn head_object(path: web::Path<(String, String, String)>) -> impl Responder {
    let (account_id, repository_id, key) = path.into_inner();

    let client = S3Client::new(Region::UsWest2);

    let request = HeadObjectRequest {
        bucket: "us-west-2.opendata.source.coop".to_string(),
        key: format!("{}/{}/{}", account_id, repository_id, key),
        ..Default::default()
    };

    match client.head_object(request).await {
        Ok(output) => {
            let content_length = output.content_length.unwrap_or_default() as usize;
            let last_modified = output.last_modified.unwrap_or_else(|| "Thu, 1 Jan 1970 00:00:00 GMT".to_string());
            HttpResponse::Ok()
                .insert_header(("Last-Modified", last_modified))
                .body(FakeBody { size: content_length })
        }
        Err(err) => {
            dbg!(&err);
            HttpResponse::NotFound().finish()
        }
    }
}

#[derive(Deserialize)]
struct ListObjectsV2Query {
    #[serde(rename = "list-type")]
    list_type: u8,
    #[serde(rename = "prefix")]
    prefix: String
}

#[get("/{account_id}")]
async fn list_objects(info: web::Query<ListObjectsV2Query>, path: web::Path<String>) -> impl Responder {
    let account_id = path.into_inner();

    let (repository_id, prefix) = split_at_first_slash(info.prefix.clone());

    match fetch_repository_client(&account_id, &repository_id).await {
        Ok(repository_client) =>  {
            match repository_client.list_objects_v2(prefix.clone()).await {
                Ok(res) => {
                    match to_string_with_root("ListBucketResult", &res) {
                        Ok(serialized) => {
                            HttpResponse::Ok()
                                .content_type("application/xml")
                                .body(serialized)
                        },
                        Err(e) => {
                            eprintln!("Serialization error: {:?}", e);
                            HttpResponse::InternalServerError().finish()
                        }
                    }
                }
                Err(_) => {
                    HttpResponse::NotFound().finish()
                }
            }
        }
        Err(_) => {
            HttpResponse::NotFound().finish()
        }
    }
}
