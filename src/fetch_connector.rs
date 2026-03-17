//! Custom `HttpConnector` for `object_store` on Cloudflare Workers.
//!
//! Uses the Workers Fetch API to make HTTP requests, bridging the `!Send`
//! JS interop boundary via channels.

use bytes::Bytes;
use futures::channel::{mpsc, oneshot};
use futures::{SinkExt, StreamExt};
use http_body::Frame;
use http_body_util::StreamBody;
use object_store::client::{
    HttpClient, HttpConnector, HttpError, HttpErrorKind, HttpRequest, HttpResponse,
    HttpResponseBody, HttpService,
};
use object_store::ClientOptions;
use wasm_bindgen_futures::spawn_local;

/// A factory for creating HTTP clients that use the Workers Fetch API.
#[derive(Debug, Default, Clone)]
pub struct FetchConnector;

impl HttpConnector for FetchConnector {
    fn connect(&self, _options: &ClientOptions) -> object_store::Result<HttpClient> {
        Ok(HttpClient::new(FetchService))
    }
}

/// HTTP service implementation using the Workers Fetch API.
///
/// Each `call()` spawns a `spawn_local` task because `worker::Fetch::send()`
/// returns a `!Send` future. A oneshot channel bridges the result back to
/// the `Send` context that `object_store` expects.
#[derive(Debug, Clone)]
struct FetchService;

impl FetchService {
    async fn do_fetch(&self, worker_req: worker::Request) -> Result<HttpResponse, HttpError> {
        let (tx, rx) = oneshot::channel();

        spawn_local(async move {
            let result = Self::fetch_inner(worker_req).await;
            let _ = tx.send(result);
        });

        rx.await.unwrap_or_else(|_| {
            Err(HttpError::new(
                HttpErrorKind::Unknown,
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "fetch channel dropped"),
            ))
        })
    }

    async fn fetch_inner(worker_req: worker::Request) -> Result<HttpResponse, HttpError> {
        let mut resp = worker::Fetch::Request(worker_req)
            .send()
            .await
            .map_err(|e| HttpError::new(HttpErrorKind::Unknown, e))?;

        let status = http::StatusCode::from_u16(resp.status_code())
            .unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR);

        // Convert response headers
        let mut headers = http::HeaderMap::new();
        let worker_headers = resp.headers();
        for (key, value) in worker_headers.entries() {
            if let (Ok(name), Ok(val)) = (
                http::header::HeaderName::try_from(key.as_str()),
                http::header::HeaderValue::try_from(value.as_str()),
            ) {
                headers.insert(name, val);
            }
        }

        // Convert body: stream via mpsc channel
        let body = match resp.stream() {
            Ok(byte_stream) => byte_stream_to_http_body(byte_stream).await,
            Err(_) => {
                // Fall back to reading body as bytes
                let body_bytes = resp
                    .bytes()
                    .await
                    .map_err(|e| HttpError::new(HttpErrorKind::Unknown, e))?;
                HttpResponseBody::from(Bytes::from(body_bytes))
            }
        };

        let mut http_response = HttpResponse::new(body);
        *http_response.status_mut() = status;
        *http_response.headers_mut() = headers;

        Ok(http_response)
    }
}

#[async_trait::async_trait]
impl HttpService for FetchService {
    async fn call(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        // Convert http::Request to worker::Request
        let method = req.method().to_string();
        let uri = req.uri().to_string();
        let headers = req.headers().clone();

        let mut worker_req = worker::Request::new(&uri, worker::Method::from(method))
            .map_err(|e| HttpError::new(HttpErrorKind::Unknown, e))?;

        // Copy headers
        {
            let worker_headers = worker_req
                .headers_mut()
                .map_err(|e| HttpError::new(HttpErrorKind::Unknown, e))?;
            for (key, value) in headers.iter() {
                if let Ok(v) = value.to_str() {
                    let _ = worker_headers.set(key.as_str(), v);
                }
            }
        }

        self.do_fetch(worker_req).await
    }
}

/// Convert a `worker::ByteStream` to an `HttpResponseBody` via mpsc channel.
///
/// The ByteStream is consumed in a `spawn_local` task (non-Send context).
/// Chunks are sent through an mpsc channel whose receiver implements `Send`,
/// which is then wrapped as a streaming `HttpResponseBody`.
async fn byte_stream_to_http_body(mut stream: worker::ByteStream) -> HttpResponseBody {
    let (mut tx, rx) = mpsc::channel(1);

    spawn_local(async move {
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    if tx.send(Ok(Bytes::from(bytes))).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx
                        .send(Err(HttpError::new(HttpErrorKind::Unknown, e)))
                        .await;
                    break;
                }
            }
        }
    });

    let framed = rx.map(|chunk| {
        let frame = Frame::data(chunk?);
        Ok(frame)
    });

    HttpResponseBody::new(StreamBody::new(framed))
}
