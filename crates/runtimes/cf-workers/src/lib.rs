//! Cloudflare Workers runtime for the S3 proxy gateway.
//!
//! This crate provides implementations of core traits using Cloudflare Workers
//! primitives. Uses the worker crate's `http` feature for standard
//! `http::Request`/`http::Response` types, eliminating manual type conversion.
//! Uses reqwest (which wraps `web_sys::fetch` on WASM) for forward execution.
//!
//! # Architecture
//!
//! ```text
//! Client -> Worker (http::Request via worker's `http` feature)
//!   -> resolve request (core resolver or Source Cooperative resolver)
//!   -> Forward: reqwest with presigned URL
//!   -> Response: LIST XML via object_store, errors, synthetic responses
//!   -> NeedsBody: multipart operations via raw signed HTTP
//! ```
//!
//! # Configuration
//!
//! On Workers, configuration is loaded from:
//! - Environment variables / secrets for simple setups
//! - Workers KV for dynamic configuration
//! - The HTTP config provider for centralized config APIs
//! - **Source Cooperative API** when `SOURCE_API_URL` is set

mod client;
mod fetch_connector;
mod tracing_layer;

use client::{FetchHttpExchange, WorkerBackend};
use source_coop_api::api::{CacheTtls, SourceApiClient};
use source_coop_api::resolver::SourceCoopResolver;
use source_coop_core::axum::{build_proxy_response, error_response};
use source_coop_core::config::static_file::{StaticConfig, StaticProvider};
use source_coop_core::oidc_backend::OidcBackendAuth;
use source_coop_core::proxy::{
    ForwardRequest, HandlerAction, ProxyHandler, RESPONSE_HEADER_ALLOWLIST,
};
use source_coop_core::resolver::{DefaultResolver, RequestResolver};
use source_coop_core::sealed_token::TokenKey;
use source_coop_oidc_provider::backend_auth::MaybeOidcAuth;
use source_coop_oidc_provider::jwt::JwtSigner;
use source_coop_oidc_provider::OidcCredentialProvider;
use source_coop_sts::{try_handle_sts, try_parse_sts_request, JwksCache};

use axum::body::Body;
use axum::response::Response;
use http::HeaderMap;
use worker::*;

/// The Worker entry point.
///
/// With the `http` feature, the worker crate provides standard `http::Request`
/// and `http::Response` types, eliminating the need for manual method/header
/// conversion.
///
/// Wrangler config (`wrangler.toml`) should bind:
/// - `CONFIG` environment variable or KV namespace for configuration
/// - `VIRTUAL_HOST_DOMAIN` environment variable (optional)
/// - `SOURCE_API_URL` + `SOURCE_API_KEY` for Source Cooperative API mode
#[event(fetch)]
async fn fetch(
    req: HttpRequest,
    env: Env,
    _ctx: Context,
) -> Result<axum::http::Response<axum::body::Body>> {
    // Initialize panic hook for better error messages
    console_error_panic_hook::set_once();

    // Initialize tracing subscriber (idempotent — ignored if already set)
    tracing::subscriber::set_global_default(tracing_layer::WorkerSubscriber::new()).ok();

    let reqwest_client = reqwest::Client::new();
    let jwks_cache = JwksCache::new(reqwest_client.clone(), std::time::Duration::from_secs(900));
    let token_key = load_token_key(&env)?;

    let (parts, worker_body) = req.into_parts();
    let body = Body::new(worker_body);
    let method = parts.method;
    let uri = parts.uri;
    let path = uri.path().to_string();
    let query = uri.query().map(|q| q.to_string());
    let headers = parts.headers;

    // Build OIDC backend auth from env secrets/vars.
    let (oidc_auth, oidc_discovery) = load_oidc_auth(&env)?;

    // Intercept OIDC discovery endpoints when OIDC provider is configured.
    if let Some(disc) = &oidc_discovery {
        if path == "/.well-known/openid-configuration" {
            let jwks_uri = format!("{}/.well-known/jwks.json", disc.issuer);
            let json = source_coop_oidc_provider::discovery::openid_configuration_json(
                &disc.issuer,
                &jwks_uri,
            );
            return Ok(Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(Body::from(json))
                .unwrap());
        }
        if path == "/.well-known/jwks.json" {
            let json = source_coop_oidc_provider::jwks::jwks_json(
                disc.signer.public_key(),
                disc.signer.kid(),
            );
            return Ok(Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(Body::from(json))
                .unwrap());
        }
    }

    // Intercept STS AssumeRoleWithWebIdentity requests before resolver dispatch.
    // STS uses STS_CONFIG (falling back to PROXY_CONFIG) for role definitions.
    if try_parse_sts_request(query.as_deref()).is_some() {
        let config = load_sts_config(&env)?;
        if let Some((status, xml)) =
            try_handle_sts(query.as_deref(), &config, &jwks_cache, token_key.as_ref()).await
        {
            return Ok(Response::builder()
                .status(status)
                .header("content-type", "application/xml")
                .body(Body::from(xml))
                .unwrap());
        }
    }

    // Source Cooperative API mode: when SOURCE_API_URL is set, resolve backends
    // dynamically from the Source API instead of static PROXY_CONFIG.
    if let Ok(source_api_url) = env.var("SOURCE_API_URL") {
        let source_api_key = env
            .var("SOURCE_API_KEY")
            .map(|v| v.to_string())
            .map_err(|e| {
                worker::Error::RustError(format!(
                    "SOURCE_API_KEY required when SOURCE_API_URL is set: {}",
                    e
                ))
            })?;

        tracing::info!(
            source_api_url = source_api_url.to_string(),
            "SOURCE_API_URL set, using Source Cooperative API resolver"
        );

        let cache_ttls = load_cache_ttls(&env);

        let api_client = SourceApiClient::new(
            client::WorkerHttpClient,
            source_api_url.to_string(),
            source_api_key,
            cache_ttls,
        );
        let resolver = SourceCoopResolver::new(api_client);
        let handler = ProxyHandler::new(WorkerBackend, resolver).with_oidc_auth(oidc_auth);

        return Ok(handle_action(
            method,
            &handler,
            &reqwest_client,
            &path,
            query.as_deref(),
            &headers,
            body,
        )
        .await);
    }

    let config = load_static_config(&env)?;
    let virtual_host_domain = env.var("VIRTUAL_HOST_DOMAIN").ok().map(|v| v.to_string());
    let resolver = DefaultResolver::new(config, virtual_host_domain, token_key);
    let handler = ProxyHandler::new(WorkerBackend, resolver).with_oidc_auth(oidc_auth);

    Ok(handle_action(
        method,
        &handler,
        &reqwest_client,
        &path,
        query.as_deref(),
        &headers,
        body,
    )
    .await)
}

// ── Two-phase request handling ──────────────────────────────────────

/// Handle the resolved action for any resolver type.
async fn handle_action<R: RequestResolver, O: OidcBackendAuth>(
    method: http::Method,
    handler: &ProxyHandler<WorkerBackend, R, O>,
    client: &reqwest::Client,
    path: &str,
    query: Option<&str>,
    headers: &http::HeaderMap,
    body: Body,
) -> Response {
    let action = handler.resolve_request(method, path, query, headers).await;

    match action {
        HandlerAction::Response(result) => build_proxy_response(result),
        HandlerAction::Forward(fwd) => forward_to_backend(client, fwd, body).await,
        HandlerAction::NeedsBody(pending) => {
            let collected = match axum::body::to_bytes(body, usize::MAX).await {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!(error = %e, "failed to read request body");
                    return error_response(500, "Internal error");
                }
            };
            let result = handler.handle_with_body(pending, collected).await;
            build_proxy_response(result)
        }
    }
}

/// Execute a Forward request via reqwest.
///
/// On WASM, reqwest wraps `web_sys::fetch` internally. Bodies are collected
/// to bytes since WASM reqwest doesn't support streaming.
async fn forward_to_backend(client: &reqwest::Client, fwd: ForwardRequest, body: Body) -> Response {
    let mut req_builder = client.request(fwd.method.clone(), fwd.url.as_str());

    for (k, v) in fwd.headers.iter() {
        req_builder = req_builder.header(k, v);
    }

    // Attach body for PUT — collect to bytes since WASM reqwest
    // doesn't support wrap_stream
    if fwd.method == http::Method::PUT {
        match axum::body::to_bytes(body, usize::MAX).await {
            Ok(bytes) => {
                req_builder = req_builder.body(bytes);
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to read PUT body");
                return error_response(500, "Internal error");
            }
        }
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

    // Read response body as bytes (WASM reqwest doesn't support bytes_stream)
    let resp_bytes = match backend_resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "failed to read backend response");
            return error_response(502, "Bad Gateway");
        }
    };

    let mut builder = Response::builder().status(status);
    for (k, v) in resp_headers.iter() {
        builder = builder.header(k, v);
    }

    builder.body(Body::from(resp_bytes)).unwrap()
}

// ── Shared helpers ──────────────────────────────────────────────────

/// Load a StaticProvider from a named env var (supports both JSON string and JS object).
fn load_config_from_env(env: &Env, var_name: &str) -> Result<StaticProvider> {
    if let Ok(var) = env.var(var_name) {
        let config_str = var.to_string();
        tracing::debug!(
            var = var_name,
            config_len = config_str.len(),
            "loaded config as string"
        );
        StaticProvider::from_json(&config_str)
            .map_err(|e| worker::Error::RustError(format!("{} config error: {}", var_name, e)))
    } else {
        tracing::debug!(var = var_name, "loading config as object");
        let static_config: StaticConfig = env
            .object_var(var_name)
            .map_err(|e| worker::Error::RustError(format!("{} config error: {}", var_name, e)))?;
        Ok(StaticProvider::from_config(static_config))
    }
}

fn load_static_config(env: &Env) -> Result<StaticProvider> {
    load_config_from_env(env, "PROXY_CONFIG")
}

/// Load the optional session token encryption key from the `SESSION_TOKEN_KEY` secret.
fn load_token_key(env: &Env) -> Result<Option<TokenKey>> {
    match env.secret("SESSION_TOKEN_KEY") {
        Ok(val) => {
            let key = TokenKey::from_base64(&val.to_string())
                .map_err(|e| worker::Error::RustError(e.to_string()))?;
            Ok(Some(key))
        }
        Err(_) => Ok(None),
    }
}

/// Load STS config: tries STS_CONFIG first, falls back to PROXY_CONFIG.
fn load_sts_config(env: &Env) -> Result<StaticProvider> {
    load_config_from_env(env, "STS_CONFIG").or_else(|_| load_config_from_env(env, "PROXY_CONFIG"))
}

type OidcAuth = MaybeOidcAuth<FetchHttpExchange>;

struct WorkerOidcDiscovery {
    issuer: String,
    signer: JwtSigner,
}

/// Load OIDC provider config from env secrets/vars.
///
/// Returns `MaybeOidcAuth::Enabled` if both `OIDC_PROVIDER_KEY` (secret) and
/// `OIDC_PROVIDER_ISSUER` (var) are set; otherwise `Disabled`.
fn load_oidc_auth(env: &Env) -> Result<(OidcAuth, Option<WorkerOidcDiscovery>)> {
    let key_pem = match env.secret("OIDC_PROVIDER_KEY") {
        Ok(val) => Some(val.to_string()),
        Err(_) => None,
    };
    let issuer = env.var("OIDC_PROVIDER_ISSUER").ok().map(|v| v.to_string());

    match (key_pem, issuer) {
        (Some(pem), Some(issuer)) => {
            let signer = JwtSigner::from_pem(&pem, "proxy-key-1".into(), 300)
                .map_err(|e| worker::Error::RustError(format!("OIDC signer error: {e}")))?;
            let http = FetchHttpExchange;
            let provider = OidcCredentialProvider::new(
                signer.clone(),
                http,
                issuer.clone(),
                "sts.amazonaws.com".into(),
            );
            let auth = MaybeOidcAuth::Enabled(
                source_coop_oidc_provider::backend_auth::AwsOidcBackendAuth::new(provider),
            );
            let discovery = WorkerOidcDiscovery { issuer, signer };
            Ok((auth, Some(discovery)))
        }
        _ => Ok((MaybeOidcAuth::Disabled, None)),
    }
}

/// Load cache TTL overrides from environment variables.
fn load_cache_ttls(env: &Env) -> CacheTtls {
    let mut cache_ttls = CacheTtls::default();
    if let Ok(v) = env.var("SOURCE_CACHE_TTL_PRODUCT") {
        if let Ok(n) = v.to_string().parse::<u32>() {
            cache_ttls.product = n;
        }
    }
    if let Ok(v) = env.var("SOURCE_CACHE_TTL_DATA_CONNECTION") {
        if let Ok(n) = v.to_string().parse::<u32>() {
            cache_ttls.data_connection = n;
        }
    }
    if let Ok(v) = env.var("SOURCE_CACHE_TTL_PERMISSIONS") {
        if let Ok(n) = v.to_string().parse::<u32>() {
            cache_ttls.permissions = n;
        }
    }
    if let Ok(v) = env.var("SOURCE_CACHE_TTL_ACCOUNT") {
        if let Ok(n) = v.to_string().parse::<u32>() {
            cache_ttls.account = n;
        }
    }
    if let Ok(v) = env.var("SOURCE_CACHE_TTL_API_KEY") {
        if let Ok(n) = v.to_string().parse::<u32>() {
            cache_ttls.api_key = n;
        }
    }
    cache_ttls
}
