//! Cloudflare Worker entrypoint for the Source Cooperative data proxy.
//!
//! Each request flows through `fetch`: parse → short-circuit (OPTIONS / writes
//! / STS-disabled) → rewrite `/{account}/{product}/{key}` to an internal
//! `account:product` bucket → dispatch through the multistore gateway → emit
//! analytics + location telemetry → apply CORS. Isolate-shared statics (HTTP
//! client, JWKS cache, OIDC provider) initialize lazily from the first
//! request's config.

mod analytics;
mod authz;
mod backend_auth;
mod config;
mod handlers;
mod location;
mod pagination;
mod source_api;
mod sts;
mod sts_cache;

use crate::source_api::{ApiAuth, SourceCoopRegistry};
use analytics::log_analytics;
use handlers::{AccountListHandler, IndexHandler};
use multistore::api::response::ErrorResponse;
use multistore::proxy::{GatewayResponse, ProxyGateway};
use multistore::route_handler::{ProxyResult, RequestInfo};
use multistore::router::Router;
use multistore_cf_workers::{
    collect_js_body, GatewayResponseExt, NoopCredentialRegistry, RequestParts, WorkerBackend,
    WorkerSubscriber,
};
use multistore_oidc_provider::backend_auth::{AwsBackendAuth, MaybeOidcAuth};
use multistore_oidc_provider::route_handler::OidcRouterExt;
use multistore_oidc_provider::{HttpExchange, OidcCredentialProvider, OidcProviderError};
use multistore_path_mapping::{MappedRegistry, PathMapping};
use multistore_sts::jwks::JwksCache;
use multistore_sts::route_handler::StsRouterExt;
use std::sync::OnceLock;
use sts::StsCredentialRegistry;
use worker::{event, Context, Env, Result};

use crate::config::load_config;

/// Separator used to join account + product into a single internal bucket name.
pub(crate) const BUCKET_SEPARATOR: &str = ":";

/// Shared `reqwest::Client` reused across requests within an isolate.
/// `reqwest::Client` is `Arc`-backed so cloning out of the cell is cheap.
static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn http_client() -> reqwest::Client {
    HTTP_CLIENT.get_or_init(reqwest::Client::new).clone()
}

/// Shared `JwksCache`. Its `entries`/`failures` maps are `Arc<Mutex<_>>` so
/// cloning the cache is cheap and shares state — the 15-minute TTL is
/// finally effective across requests.
static JWKS_CACHE: OnceLock<JwksCache> = OnceLock::new();

fn jwks_cache() -> JwksCache {
    JWKS_CACHE
        .get_or_init(|| JwksCache::new(http_client(), std::time::Duration::from_secs(900)))
        .clone()
}

/// Bound the outbound STS `AssumeRoleWithWebIdentity` call. Without it a slow or
/// hung federation lets the whole request stall until the Cloudflare edge kills
/// it with a non-XML `error code: NNNN` body, which the caller's AWS SDK can't
/// deserialize ("char 'e' is not expected.:1:1"). With the bound, a stall instead
/// returns a proper S3 `ServiceUnavailable` XML error (HttpError → BackendError
/// → 503) the client can parse and retry. STS normally answers in well under a
/// second, so this only trips on genuine stalls — which only happen on a cold
/// isolate, since the OIDC provider caches credentials across requests once warm.
// ponytail: fixed 10s; promote to an env var if a deployment ever needs to tune it.
const STS_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// [`HttpExchange`] for outbound STS calls, backed by the shared reqwest client
/// (reqwest wraps `web_sys::fetch` on wasm). This is what lets the OIDC
/// backend-auth middleware POST `AssumeRoleWithWebIdentity` to AWS STS.
#[derive(Clone)]
struct FetchHttpExchange {
    client: reqwest::Client,
}

impl HttpExchange for FetchHttpExchange {
    async fn post_form(
        &self,
        url: &str,
        form: &[(&str, &str)],
    ) -> std::result::Result<String, OidcProviderError> {
        // L2 (cross-isolate, per-colo) cache for the AssumeRoleWithWebIdentity
        // response, keyed by RoleArn. On a hit we skip the slow STS round-trip
        // entirely — the one thing that stalls the request hot path on a cold
        // isolate. multistore's in-isolate cache is L1; this sits under it.
        let cache_key = sts_cache::role_arn_from_form(form).map(sts_cache::cache_key);
        if let Some(ref key) = cache_key {
            if let Some(body) = sts_cache_get(key).await {
                return Ok(body);
            }
        }

        let resp = self
            .client
            .post(url)
            .form(form)
            .timeout(STS_REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| OidcProviderError::HttpError(e.to_string()))?;
        // Intentionally NOT checking the HTTP status / calling
        // `error_for_status()`: AWS STS returns its `<ErrorResponse>` XML in the
        // body on 4xx/5xx, and multistore's `parse_response` reads the error
        // (code + message) out of that body. Discarding it on a non-2xx would
        // lose the diagnostic and the precise ProxyError mapping.
        let body = resp
            .text()
            .await
            .map_err(|e| OidcProviderError::HttpError(e.to_string()))?;

        // Cache only a successful, not-near-expiry credential response —
        // `ttl_secs` returns None for an STS error document, so failures are
        // never cached.
        if let Some(ref key) = cache_key {
            let now = (worker::Date::now().as_millis() / 1000) as i64;
            if let Some(ttl) = sts_cache::ttl_secs(&body, now) {
                sts_cache_put(key, &body, ttl).await;
            }
        }
        Ok(body)
    }
}

/// L2 read. Best-effort: any cache error degrades to a miss (we just call STS).
async fn sts_cache_get(key: &str) -> Option<String> {
    let mut resp = worker::Cache::default().get(key, false).await.ok()??;
    resp.text().await.ok()
}

/// L2 write. Best-effort: a failed put just means the next request re-mints.
async fn sts_cache_put(key: &str, body: &str, ttl_secs: u32) {
    let headers = worker::Headers::new();
    let _ = headers.set("content-type", "text/xml");
    let _ = headers.set("cache-control", &format!("max-age={ttl_secs}"));
    match worker::Response::ok(body) {
        Ok(resp) => {
            if let Err(e) = worker::Cache::default()
                .put(key, resp.with_headers(headers))
                .await
            {
                tracing::warn!("STS L2 cache put failed: {e}");
            }
        }
        Err(e) => tracing::warn!("STS L2 cache response build failed: {e}"),
    }
}

/// Isolate-shared OIDC credential provider for backend federation. The gateway
/// (and its middleware) are rebuilt per request, but the provider — and its
/// credential cache — must persist so the proxy doesn't re-mint a JWT and re-run
/// `AssumeRoleWithWebIdentity` on every request to the same role. Initialized
/// from the first request's signing config, which is constant for the isolate.
static OIDC_PROVIDER: OnceLock<OidcCredentialProvider<FetchHttpExchange>> = OnceLock::new();

#[event(fetch)]
async fn fetch(req: web_sys::Request, env: Env, ctx: Context) -> Result<web_sys::Response> {
    console_error_panic_hook::set_once();
    let max_level = init_tracing(&env);
    let config = load_config(&env);

    // ── Parse request ──────────────────────────────────────────────
    let (mut parts, js_body) = RequestParts::from_web_sys(&req)
        .map_err(|e| worker::Error::RustError(format!("invalid request: {e}")))?;

    // The router matches `/.sts` exactly; a trailing-slash variant would
    // otherwise fall through to bucket mapping and 404 confusingly.
    if parts.path == "/.sts/" {
        parts.path.pop();
    }

    let request_id = extract_request_id(&parts.headers);

    // Special endpoints (OIDC discovery, STS token exchange) manage their own
    // methods and bypass the S3 object/bucket path mapping below.
    let is_special_path = parts.path.starts_with("/.well-known/") || parts.path == "/.sts";

    // ── Short-circuit: OPTIONS preflight ────────────────────────────
    if parts.method == http::Method::OPTIONS {
        let init = web_sys::ResponseInit::new();
        init.set_status(204);
        let resp = web_sys::Response::new_with_opt_str_and_init(None, &init)
            .unwrap_or_else(|_| web_sys::Response::new().unwrap());
        return Ok(add_cors(resp));
    }

    // Writes (PUT/POST/DELETE) flow through the gateway: the registry authorizes
    // them (caller must hold product write permission; the connection must be
    // writable and signable) and the backend-auth middleware signs them. See
    // `authz` and `backend_auth`.

    // ── Short-circuit: STS disabled (fail closed) ───────────────────
    // `/.sts` requires an audience restriction (AUTH_AUDIENCE) to be safe —
    // without it, an ID token minted for any OAuth client of AUTH_ISSUER could
    // be exchanged for a user's credentials. When unset, refuse the endpoint
    // with a 501 rather than serving it unrestricted.
    if parts.path == "/.sts" && config.auth_audiences.is_empty() {
        let resp = ErrorResponse {
            code: "NotImplemented".to_string(),
            message: "STS token exchange is not configured".to_string(),
            resource: String::new(),
            request_id: request_id.clone(),
        };
        return Ok(add_cors(
            GatewayResponse::Response(ProxyResult::xml(501, resp.to_xml())).into_web_sys(),
        ));
    }

    // ── Path rewriting ─────────────────────────────────────────────
    // Source Cooperative path mapping: `/{account}/{product}/{key}`
    // → internal bucket `account:product`, display name shows just `account`.
    let mapping = PathMapping {
        bucket_segments: 2,
        bucket_separator: BUCKET_SEPARATOR.to_string(),
        display_bucket_segments: 1,
    };
    let rewrite = if is_special_path {
        // Special endpoints aren't S3 paths — pass them through unrewritten.
        multistore_path_mapping::RewriteResult {
            path: parts.path.clone(),
            query: parts.query.clone(),
            signing_path: parts.path.clone(),
            signing_query: parts.query.clone(),
        }
    } else {
        mapping.rewrite_request(&parts.path, parts.query.as_deref())
    };

    // ── Build API auth ─────────────────────────────────────────────
    let api_auth = ApiAuth::new(
        config.oidc.signer.clone(),
        config.oidc.issuer.clone(),
        config.api_base_url.clone(),
    );

    // ── Build gateway with route handlers ──────────────────────────
    let registry = SourceCoopRegistry::new(
        config.api_base_url.clone(),
        api_auth.clone(),
        request_id.clone(),
    );

    // ── Build router ─────────────────────────────────────────────
    let mut router = Router::new().with_oidc_discovery(
        config.oidc.issuer.clone(),
        std::iter::once(config.oidc.signer.clone())
            .chain(config.oidc.previous_signer.clone())
            .collect(),
    );

    // Mount STS token exchange only when an audience restriction is configured.
    // The unset case is refused by the fail-closed 501 short-circuit above, so
    // an unrestricted exchanger is never registered.
    if !config.auth_audiences.is_empty() {
        let sts_registry = StsCredentialRegistry::new(
            config.auth_issuer.clone(),
            config.auth_audiences.clone(),
            config.sts_max_session_duration_secs,
        );
        router = router.with_sts(
            "/.sts",
            sts_registry,
            jwks_cache(),
            Some(config.session_token_key.clone()),
        );
    }

    let router = router
        .route("/", IndexHandler)
        .route("/{bucket}", AccountListHandler::new(registry.clone()));

    // ── Backend federation middleware ─────────────────────────────
    // For a connection resolved with auth_type=oidc, mint the proxy's OIDC
    // assertion, exchange it at AWS STS (AssumeRoleWithWebIdentity) over fetch,
    // and inject the temporary credentials so the backend request is signed.
    // A no-op for connections without auth_type=oidc (i.e. unsigned/public).
    // Reuse the isolate-shared provider so its credential cache stays warm across
    // requests; `clone()` is cheap and shares that cache.
    let provider = OIDC_PROVIDER
        .get_or_init(|| {
            OidcCredentialProvider::new(
                config.oidc.signer.clone(),
                FetchHttpExchange {
                    client: http_client(),
                },
                config.oidc.issuer.clone(),
                crate::backend_auth::AWS_STS_AUDIENCE.to_string(),
            )
        })
        .clone();
    let backend_auth = MaybeOidcAuth::Enabled(Box::new(AwsBackendAuth::new(provider)));

    let gateway = ProxyGateway::new(
        WorkerBackend,
        MappedRegistry::new(registry, mapping.clone()),
        NoopCredentialRegistry,
        None,
    )
    .with_middleware(backend_auth)
    .with_router(router)
    .with_debug_errors(max_level >= tracing::Level::DEBUG)
    .with_credential_resolver(config.session_token_key.clone());

    // ── Dispatch through gateway ──────────────────────────────────
    let span =
        tracing::info_span!("request", %request_id, method = %parts.method, path = %parts.path);
    let _guard = span.enter();

    let request_info = RequestInfo::new(
        &parts.method,
        &rewrite.path,
        rewrite.query.as_deref(),
        &parts.headers,
        None,
    )
    .with_signing_path(&rewrite.signing_path)
    .with_signing_query(rewrite.signing_query.as_deref());

    let start_ms = js_sys::Date::now();
    let response = gateway
        .handle_request(&request_info, js_body, collect_js_body)
        .await
        .into_web_sys();
    let duration_ms = js_sys::Date::now() - start_ms;
    tracing::info!(status = response.status(), duration_ms, "response");

    // ── Extract path segments (used by analytics + location broadcast) ──
    let (account, product, key) = extract_path_segments(&parts.path);

    // ── Analytics ───────────────────────────────────────────────
    // Special endpoints (`/.well-known/*`, `/.sts`) aren't product requests;
    // logging them would pollute the dataset with account = ".well-known".
    if !parts.path.starts_with("/.") {
        log_analytics(
            &env,
            &parts.headers,
            &response,
            &parts.method,
            account,
            product,
            key,
            duration_ms,
            &config.ip_hash_salt,
        );
    }

    // ── Broadcast location to WebSocket viewers ──────────────────
    // Only successful GET reads of a real product (not /.well-known or /.sts).
    if let (&http::Method::GET, Some(acct), Some(prod)) = (&parts.method, account, product) {
        if response.status() < 400 && !parts.path.starts_with("/.") {
            location::maybe_broadcast_location(
                &ctx,
                &env,
                location::LocationEvent {
                    cf: location::CfProperties::from_request(&req),
                    country: header_str(&parts.headers, "cf-ipcountry").to_string(),
                    account: acct.to_string(),
                    product: prod.to_string(),
                    key: key.unwrap_or("").to_string(),
                    api_base_url: config.api_base_url.clone(),
                    api_auth: api_auth.clone(),
                },
            );
        }
    }

    let response = add_cors(response);
    if !request_id.is_empty() {
        let _ = response.headers().set("x-request-id", &request_id);
    }
    Ok(response)
}

// ── Helpers ─────────────────────────────────────────────────────────

fn init_tracing(env: &Env) -> tracing::Level {
    let max_level = env
        .var("LOG_LEVEL")
        .map(|v| v.to_string())
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(tracing::Level::WARN);
    tracing::subscriber::set_global_default(WorkerSubscriber::new().with_max_level(max_level)).ok();
    max_level
}

fn extract_request_id(headers: &http::HeaderMap) -> String {
    headers
        .get("cf-ray")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

pub(crate) fn header_str<'a>(headers: &'a http::HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

/// Split `/{account}/{product}[/{key}]` into its segments; any segment not
/// present is `None`. Used to tag the analytics event and the location broadcast.
fn extract_path_segments(path: &str) -> (Option<&str>, Option<&str>, Option<&str>) {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return (None, None, None);
    }
    let mut parts = trimmed.splitn(3, '/');
    let account = parts.next();
    let product = parts.next();
    let key = parts.next();
    (account, product, key)
}

// ── CORS ────────────────────────────────────────────────────────────

fn add_cors(resp: web_sys::Response) -> web_sys::Response {
    let h = resp.headers();
    for (name, value) in [
        ("access-control-allow-origin", "*"),
        (
            "access-control-allow-methods",
            "GET, HEAD, PUT, POST, DELETE, OPTIONS",
        ),
        ("access-control-allow-headers", "*"),
        ("access-control-expose-headers", "*"),
    ] {
        if let Err(e) = h.set(name, value) {
            tracing::warn!("failed to set CORS header {}: {:?}", name, e);
        }
    }
    resp
}
