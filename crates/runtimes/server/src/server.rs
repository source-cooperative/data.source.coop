//! HTTP server using Hyper, wiring everything together.

use crate::body::{build_hyper_response, ServerResponseBody};
use crate::client::ServerBackend;
use bytes::Bytes;
use http::{Request, Response};
use http_body_util::{BodyExt, Either, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use s3_proxy_core::config::ConfigProvider;
use s3_proxy_core::proxy::ProxyHandler;
use s3_proxy_core::resolver::DefaultResolver;
use std::net::SocketAddr;
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
    let resolver = DefaultResolver::new(config, server_config.virtual_host_domain);
    let handler = Arc::new(ProxyHandler::new(backend, resolver));

    let listener = TcpListener::bind(server_config.listen_addr).await?;
    tracing::info!("listening on {}", server_config.listen_addr);

    loop {
        let (stream, remote_addr) = listener.accept().await?;
        let handler = handler.clone();

        tokio::spawn(async move {
            let service = service_fn(move |req: Request<Incoming>| {
                let handler = handler.clone();

                async move {
                    tracing::debug!(
                        remote_addr = %remote_addr,
                        method = %req.method(),
                        uri = %req.uri(),
                        "incoming connection"
                    );
                    let result = handle_hyper_request(req, &handler).await;
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
    req: Request<Incoming>,
    handler: &ProxyHandler<ServerBackend, R>,
) -> Result<Response<ServerResponseBody>, Box<dyn std::error::Error + Send + Sync>>
where
    R: s3_proxy_core::resolver::RequestResolver + Send + Sync,
{
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path();
    let query = uri.query();
    let headers = req.headers().clone();

    // Materialize incoming body to Bytes
    let incoming_bytes = req.into_body().collect().await?.to_bytes();

    let result = handler
        .handle_request(method, path, query, &headers, incoming_bytes)
        .await;

    build_hyper_response(result)
}
