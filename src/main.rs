use actix_web::{
    body::{BodySize, MessageBody},
    http::header::RANGE,
    get, head, web, App, Error as ActixError, HttpRequest, HttpResponse, HttpServer, Responder,
};
use futures::{Stream, TryStreamExt};
use pin_project_lite::pin_project;
use rusoto_core::Region;
use rusoto_dynamodb::{AttributeValue, DynamoDb, DynamoDbClient, GetItemInput};
use rusoto_s3::{GetObjectRequest, HeadObjectRequest, S3Client, S3};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

pin_project! {
    pub struct S3ObjectStream<S> {
        #[pin]
        inner: S,
        size: u64,
    }
}

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
    ) -> Poll<Option<Result<web::Bytes, actix_web::Error>>> {
        Poll::Ready(None)
    }
}

impl<S> S3ObjectStream<S> {
    pub fn new(inner: S, size: u64) -> Self {
        Self { inner, size }
    }
}

impl<S> MessageBody for S3ObjectStream<S>
where
    S: Stream,
    S::Item: Into<Result<web::Bytes, ActixError>>,
{
    type Error = ActixError;

    fn size(&self) -> BodySize {
        BodySize::Sized(self.size)
    }

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<web::Bytes, Self::Error>>> {
        let this = self.project();
        match this.inner.poll_next(cx) {
            Poll::Ready(Some(item)) => Poll::Ready(Some(item.into())),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

fn attribute_value_to_json(attr: AttributeValue) -> Value {
    match attr {
        AttributeValue { s: Some(s), .. } => Value::String(s),
        AttributeValue { n: Some(n), .. } => n.parse().map(Value::Number).unwrap_or(Value::Null),
        AttributeValue { bool: Some(b), .. } => Value::Bool(b),
        AttributeValue { m: Some(m), .. } => {
            let mut map = Map::new();
            for (k, v) in m {
                map.insert(k, attribute_value_to_json(v));
            }
            Value::Object(map)
        }
        AttributeValue { l: Some(l), .. } => {
            Value::Array(l.into_iter().map(attribute_value_to_json).collect())
        }
        // Add other AttributeValue types as needed
        _ => Value::Null,
    }
}

async fn get_repository_record(account_id: &String, repository_id: &String) -> Result<String, Box<dyn std::error::Error>> {
    let client = DynamoDbClient::new(Region::default());

    let mut key = HashMap::new();
    key.insert(
        "account_id".to_string(),
        AttributeValue {
            s: Some(account_id.to_string()),
            ..Default::default()
        },
    );
    key.insert(
        "repository_id".to_string(),
        AttributeValue {
            s: Some(repository_id.to_string()),
            ..Default::default()
        },
    );

    let input = GetItemInput {
        table_name: "source-cooperative-repositories".to_string(),
        key,
        ..Default::default()
    };

    match client.get_item(input).await {
        Ok(output) => {
            if let Some(item) = output.item {
                let json_value: Value = Value::Object(
                    item.into_iter()
                        .map(|(k, v)| (k, attribute_value_to_json(v)))
                        .collect(),
                );

                // Convert the JSON Value to a pretty-printed string
                let _json_string = serde_json::to_string_pretty(&json_value)?;
                // println!("Item found:\n{}", json_string);
            } else {
                println!("No item found with the specified keys.");
            }
        }
        Err(error) => {
            println!("Error: {:?}", error);
        }
    }

    Ok("foobar".to_string())
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    HttpServer::new(|| App::new().service(get_readme).service(get_object).service(head_object))
        .bind("0.0.0.0:8080")?
        .run()
        .await
}

#[get("/{account_id}/{repository_id}/READMEFOO.md")]
async fn get_readme(path: web::Path<(String, String)>) -> impl Responder {
    let (account_id, repository_id) = path.into_inner();
    dbg!(&account_id);
    let json_value = json!({
        "account_id": account_id,
        "repository_id": repository_id
    });

    HttpResponse::Ok()
        .content_type("application/json")
        .body(json_value.to_string())
}

#[get("/{account_id}/{repository_id}/{key:.*}")]
async fn get_object(req: HttpRequest, path: web::Path<(String, String, String)>) -> impl Responder {
    let (account_id, repository_id, key) = path.into_inner();
    let headers = req.headers();

    //if let Some(_range) = headers.get("Range") {
        //println!("Range: {:?}", range);
        //}

    match get_repository_record(&account_id, &repository_id).await {
        Ok(_res) => {
            // println!("{}", res);
        }
        Err(_) => {
            return HttpResponse::InternalServerError().finish();
        }
    }

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
            let last_modified = output.last_modified.unwrap_or_else(|| "Wed, 21 Oct 2015 07:28:00 GMT".to_string());

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
            let last_modified = output.last_modified.unwrap_or_else(|| "Wed, 21 Oct 2015 07:28:00 GMT".to_string());
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
