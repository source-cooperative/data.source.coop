//! HTTP server using axum, wiring everything together.

use crate::client::{ReqwestHttpExchange, ServerBackend};
use axum::body::Body;
use axum::extract::State;
use axum::response::Response;
use axum::Router;
use futures::TryStreamExt;
use http::HeaderMap;
use http_body_util::BodyStream;
use source_coop_core::axum::{build_proxy_response, error_response};
use source_coop_core::config::ConfigProvider;
use source_coop_core::proxy::{
    ForwardRequest, HandlerAction, ProxyHandler, RESPONSE_HEADER_ALLOWLIST,
};
use source_coop_core::resolver::DefaultResolver;
use source_coop_core::sealed_token::TokenKey;
use source_coop_oidc_provider::backend_auth::MaybeOidcAuth;
use source_coop_oidc_provider::jwt::JwtSigner;
use source_coop_oidc_provider::OidcCredentialProvider;
use source_coop_sts::{try_handle_sts, JwksCache};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

/// Server configuration.
pub struct ServerConfig {
    pub listen_addr: SocketAddr,
    /// The base domain for virtual-hosted-style requests (e.g., "s3.example.com").
    /// If set, requests to `{bucket}.s3.example.com` use virtual-hosted style.
    pub virtual_host_domain: Option<String>,
    /// Optional AES-256-GCM key for self-contained encrypted session tokens.
    pub token_key: Option<TokenKey>,
    /// PEM-encoded RSA private key for OIDC provider (minting JWTs for backend auth).
    pub oidc_provider_key: Option<String>,
    /// Issuer URL for the OIDC provider (must be publicly reachable for JWKS discovery).
    pub oidc_provider_issuer: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen_addr: ([0, 0, 0, 0], 8080).into(),
            virtual_host_domain: None,
            token_key: None,
            oidc_provider_key: None,
            oidc_provider_issuer: None,
        }
    }
}

type OidcAuth = MaybeOidcAuth<ReqwestHttpExchange>;

struct AppState<P: ConfigProvider> {
    handler: ProxyHandler<ServerBackend, DefaultResolver<P>, OidcAuth>,
    reqwest_client: reqwest::Client,
    sts_config: P,
    jwks_cache: JwksCache,
    token_key: Option<TokenKey>,
    /// OIDC discovery data (issuer + signer), set when OIDC provider is configured.
    oidc_discovery: Option<OidcDiscovery>,
}

struct OidcDiscovery {
    issuer: String,
    signer: JwtSigner,
}

/// Run the S3 proxy server.
///
/// # Example
///
/// ```rust,ignore
/// use source_coop_core::config::static_file::StaticProvider;
/// use source_coop_server::server::{run, ServerConfig};
///
/// #[tokio::main]
/// async fn main() {
///     let config = StaticProvider::from_file("config.toml").unwrap();
///     let sts_config = config.clone();
///     let server_config = ServerConfig {
///         listen_addr: ([0, 0, 0, 0], 8080).into(),
///         virtual_host_domain: Some("s3.local".to_string()),
///         ..Default::default()
///     };
///     run(config, sts_config, server_config).await.unwrap();
/// }
/// ```
pub async fn run<P>(
    config: P,
    sts_config: P,
    server_config: ServerConfig,
) -> Result<(), Box<dyn std::error::Error>>
where
    P: ConfigProvider + Send + Sync + 'static,
{
    let backend = ServerBackend::new();
    let reqwest_client = backend.client().clone();
    let jwks_cache = JwksCache::new(reqwest_client.clone(), Duration::from_secs(900));
    let token_key = server_config.token_key;
    let resolver =
        DefaultResolver::new(config, server_config.virtual_host_domain, token_key.clone());

    // Build OIDC provider if both key and issuer are configured.
    let (oidc_auth, oidc_discovery) = match (
        &server_config.oidc_provider_key,
        &server_config.oidc_provider_issuer,
    ) {
        (Some(key_pem), Some(issuer)) => {
            let signer = JwtSigner::from_pem(key_pem, "proxy-key-1".into(), 300)
                .map_err(|e| format!("failed to create OIDC JWT signer: {e}"))?;
            let http = ReqwestHttpExchange::new(reqwest_client.clone());
            let provider = OidcCredentialProvider::new(
                signer.clone(),
                http,
                issuer.clone(),
                "sts.amazonaws.com".into(),
            );
            let auth = MaybeOidcAuth::Enabled(Box::new(
                source_coop_oidc_provider::backend_auth::AwsOidcBackendAuth::new(provider),
            ));
            let discovery = OidcDiscovery {
                issuer: issuer.clone(),
                signer,
            };
            (auth, Some(discovery))
        }
        _ => (MaybeOidcAuth::Disabled, None),
    };

    let handler = ProxyHandler::new(backend, resolver).with_oidc_auth(oidc_auth);

    let state = Arc::new(AppState {
        handler,
        reqwest_client,
        sts_config,
        jwks_cache,
        token_key,
        oidc_discovery,
    });

    let app = Router::new()
        .fallback(request_handler::<P>)
        .with_state(state);

    let listener = TcpListener::bind(server_config.listen_addr).await?;
    tracing::info!("listening on {}", server_config.listen_addr);

    axum::serve(listener, app).await?;
    Ok(())
}

async fn request_handler<P: ConfigProvider + Send + Sync + 'static>(
    State(state): State<Arc<AppState<P>>>,
    req: axum::extract::Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let method = parts.method;
    let uri = parts.uri;
    let path = uri.path().to_string();
    let query = uri.query().map(|q| q.to_string());
    let headers = parts.headers;

    tracing::debug!(
        method = %method,
        uri = %uri,
        "incoming request"
    );

    // Intercept OIDC discovery endpoints when OIDC provider is configured.
    if let Some(disc) = &state.oidc_discovery {
        if path == "/.well-known/openid-configuration" {
            let jwks_uri = format!("{}/.well-known/jwks.json", disc.issuer);
            let json = source_coop_oidc_provider::discovery::openid_configuration_json(
                &disc.issuer,
                &jwks_uri,
            );
            return Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(Body::from(json))
                .unwrap();
        }
        if path == "/.well-known/jwks.json" {
            let json = source_coop_oidc_provider::jwks::jwks_json(
                disc.signer.public_key(),
                disc.signer.kid(),
            );
            return Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(Body::from(json))
                .unwrap();
        }
    }

    // Intercept STS AssumeRoleWithWebIdentity requests
    if let Some((status, xml)) = try_handle_sts(
        query.as_deref(),
        &state.sts_config,
        &state.jwks_cache,
        state.token_key.as_ref(),
    )
    .await
    {
        return Response::builder()
            .status(status)
            .header("content-type", "application/xml")
            .body(Body::from(xml))
            .unwrap();
    }

    let action = state
        .handler
        .resolve_request(method, &path, query.as_deref(), &headers)
        .await;

    match action {
        HandlerAction::Response(result) => build_proxy_response(result),
        HandlerAction::Forward(fwd) => forward_to_backend(&state.reqwest_client, fwd, body).await,
        HandlerAction::NeedsBody(pending) => {
            let collected = match axum::body::to_bytes(body, usize::MAX).await {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!(error = %e, "failed to read request body");
                    return error_response(500, "Internal error");
                }
            };
            let result = state.handler.handle_with_body(pending, collected).await;
            build_proxy_response(result)
        }
    }
}

/// Execute a Forward request via reqwest, streaming both request and response bodies.
async fn forward_to_backend(client: &reqwest::Client, fwd: ForwardRequest, body: Body) -> Response {
    let mut req_builder = client.request(fwd.method.clone(), fwd.url.as_str());

    for (k, v) in fwd.headers.iter() {
        req_builder = req_builder.header(k, v);
    }

    // Attach streaming body for PUT
    if fwd.method == http::Method::PUT {
        let body_stream =
            BodyStream::new(body).try_filter_map(|frame| async move { Ok(frame.into_data().ok()) });
        req_builder = req_builder.body(reqwest::Body::wrap_stream(body_stream));
    }

    let backend_resp = match req_builder.send().await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!(error = %e, "forward request failed");
            return error_response(502, "Bad Gateway");
        }
    };

    let status = backend_resp.status().as_u16();

    // Forward allowlisted response headers
    let mut resp_headers = HeaderMap::new();
    for name in RESPONSE_HEADER_ALLOWLIST {
        if let Some(v) = backend_resp.headers().get(*name) {
            resp_headers.insert(*name, v.clone());
        }
    }

    // Stream the response body
    let body = Body::from_stream(backend_resp.bytes_stream());

    let mut builder = Response::builder().status(status);
    for (k, v) in resp_headers.iter() {
        builder = builder.header(k, v);
    }

    builder.body(body).unwrap()
}
