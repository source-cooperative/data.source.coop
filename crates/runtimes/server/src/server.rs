//! HTTP server using Hyper, wiring everything together.

use crate::body::{build_hyper_response, ServerResponseBody};
use crate::client::ServerBackend;
use bytes::Bytes;
use futures::{Stream, TryStreamExt};
use http::{HeaderMap, Response};
use http_body_util::{BodyExt, BodyStream, Either, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use s3_proxy_core::config::ConfigProvider;
use s3_proxy_core::proxy::{ForwardRequest, HandlerAction, ProxyHandler, RESPONSE_HEADER_ALLOWLIST};
use s3_proxy_core::resolver::DefaultResolver;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::TcpListener;

/// Server configuration.
pub struct ServerConfig {
    pub listen_addr: SocketAddr,
    /// The base domain for virtual-hosted-style requests (e.g., "s3.example.com").
    /// If set, requests to `{bucket}.s3.example.com` use virtual-hosted style.
    pub virtual_host_domain: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen_addr: ([0, 0, 0, 0], 8080).into(),
            virtual_host_domain: None,
        }
    }
}

/// Run the S3 proxy server.
///
/// # Example
///
/// ```rust,ignore
/// use s3_proxy_core::config::static_file::StaticProvider;
/// use s3_proxy_server::server::{run, ServerConfig};
///
/// #[tokio::main]
/// async fn main() {
///     let config = StaticProvider::from_file("config.toml").unwrap();
///     let server_config = ServerConfig {
///         listen_addr: ([0, 0, 0, 0], 8080).into(),
///         virtual_host_domain: Some("s3.local".to_string()),
///     };
///     run(config, server_config).await.unwrap();
/// }
/// ```
pub async fn run<P>(config: P, server_config: ServerConfig) -> Result<(), Box<dyn std::error::Error>>
where
    P: ConfigProvider + Send + Sync + 'static,
{
    let backend = ServerBackend::new();
    let reqwest_client = backend.client().clone();
    let resolver = DefaultResolver::new(config, server_config.virtual_host_domain);
    let handler = Arc::new(ProxyHandler::new(backend, resolver));

    let listener = TcpListener::bind(server_config.listen_addr).await?;
    tracing::info!("listening on {}", server_config.listen_addr);

    loop {
        let (stream, remote_addr) = listener.accept().await?;
        let handler = handler.clone();
        let client = reqwest_client.clone();

        tokio::spawn(async move {
            let service = service_fn(move |req: http::Request<Incoming>| {
                let handler = handler.clone();
                let client = client.clone();

                async move {
                    tracing::debug!(
                        remote_addr = %remote_addr,
                        method = %req.method(),
                        uri = %req.uri(),
                        "incoming connection"
                    );
                    let result = handle_hyper_request(req, &handler, &client).await;
                    match result {
                        Ok(resp) => Ok::<_, hyper::Error>(resp),
                        Err(e) => {
                            tracing::error!(remote_addr = %remote_addr, error = %e, "handler error");
                            let body = Full::new(Bytes::from(format!("Internal error: {}", e)));
                            Ok(Response::builder()
                                .status(500)
                                .body(Either::Right(Either::Left(body)))
                                .unwrap())
                        }
                    }
                }
            });

            if let Err(err) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(stream), service)
                .await
            {
                tracing::error!(remote_addr = %remote_addr, error = %err, "connection error");
            }
        });
    }
}

async fn handle_hyper_request<R>(
    req: http::Request<Incoming>,
    handler: &ProxyHandler<ServerBackend, R>,
    client: &reqwest::Client,
) -> Result<Response<ServerResponseBody>, Box<dyn std::error::Error + Send + Sync>>
where
    R: s3_proxy_core::resolver::RequestResolver + Send + Sync,
{
    let (parts, incoming_body) = req.into_parts();
    let method = parts.method;
    let uri = parts.uri;
    let path = uri.path().to_string();
    let query = uri.query().map(|q| q.to_string());
    let headers = parts.headers;

    let action = handler
        .resolve_request(method, &path, query.as_deref(), &headers)
        .await;

    match action {
        HandlerAction::Response(result) => build_hyper_response(result),
        HandlerAction::Forward(fwd) => {
            forward_to_backend(client, fwd, incoming_body).await
        }
        HandlerAction::NeedsBody(pending) => {
            let body = incoming_body.collect().await?.to_bytes();
            let result = handler.handle_with_body(pending, body).await;
            build_hyper_response(result)
        }
    }
}

/// Execute a Forward request via reqwest, streaming both request and response bodies.
async fn forward_to_backend(
    client: &reqwest::Client,
    fwd: ForwardRequest,
    incoming_body: Incoming,
) -> Result<Response<ServerResponseBody>, Box<dyn std::error::Error + Send + Sync>> {
    let mut req_builder = client.request(fwd.method.clone(), fwd.url.as_str());

    for (k, v) in fwd.headers.iter() {
        req_builder = req_builder.header(k, v);
    }

    // Attach streaming body for PUT
    if fwd.method == http::Method::PUT {
        let body_stream = BodyStream::new(incoming_body)
            .try_filter_map(|frame| async move {
                Ok(frame.into_data().ok())
            });
        req_builder = req_builder.body(reqwest::Body::wrap_stream(body_stream));
    }

    let backend_resp = req_builder.send().await.map_err(|e| {
        tracing::error!(error = %e, "forward request failed");
        Box::new(e) as Box<dyn std::error::Error + Send + Sync>
    })?;

    let status = backend_resp.status().as_u16();

    // Forward allowlisted response headers
    let mut resp_headers = HeaderMap::new();
    for name in RESPONSE_HEADER_ALLOWLIST {
        if let Some(v) = backend_resp.headers().get(*name) {
            resp_headers.insert(*name, v.clone());
        }
    }

    // Stream the response body
    let body_stream = backend_resp.bytes_stream();
    let framed = body_stream
        .map_ok(Frame::data)
        .map_err(|e| std::io::Error::other(e.to_string()));
    let body: ServerResponseBody = Either::Left(StreamBody::new(
        Box::pin(framed) as Pin<Box<dyn Stream<Item = Result<Frame<Bytes>, std::io::Error>> + Send>>
    ));

    let mut builder = Response::builder().status(status);
    for (k, v) in resp_headers.iter() {
        builder = builder.header(k, v);
    }

    Ok(builder.body(body)?)
}
