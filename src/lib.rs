mod analytics;
mod api_auth;
mod authz;
mod backend_auth;
mod cache;
mod config;
mod handlers;
mod pagination;
mod registry;
mod sts;

use crate::api_auth::ApiAuth;
use analytics::{extract_path_segments, log_request, RequestEvent};
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
use registry::SourceCoopRegistry;
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
        let resp = self
            .client
            .post(url)
            .form(form)
            .send()
            .await
            .map_err(|e| OidcProviderError::HttpError(e.to_string()))?;
        // Intentionally NOT checking the HTTP status / calling
        // `error_for_status()`: AWS STS returns its `<ErrorResponse>` XML in the
        // body on 4xx/5xx, and multistore's `parse_response` reads the error
        // (code + message) out of that body. Discarding it on a non-2xx would
        // lose the diagnostic and the precise ProxyError mapping.
        resp.text()
            .await
            .map_err(|e| OidcProviderError::HttpError(e.to_string()))
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

    let response = gateway
        .handle_request(&request_info, js_body, collect_js_body)
        .await
        .into_web_sys();
    tracing::info!(status = response.status(), "response");

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
        );
    }

    // ── Broadcast location to WebSocket viewers ──────────────────
    if let (&http::Method::GET, Some(acct), Some(prod)) = (&parts.method, account, product) {
        if response.status() < 400 && !parts.path.starts_with("/.") {
            maybe_broadcast_location(
                &ctx,
                &env,
                LocationEvent {
                    cf: CfProperties::from_request(&req),
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

fn header_str<'a>(headers: &'a http::HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

// ── Analytics ──────────────────────────────────────────────────────

fn log_analytics(
    env: &Env,
    headers: &http::HeaderMap,
    response: &web_sys::Response,
    method: &http::Method,
    account: Option<&str>,
    product: Option<&str>,
    key: Option<&str>,
) {
    let content_type = response
        .headers()
        .get("content-type")
        .ok()
        .flatten()
        .unwrap_or_default();
    let bytes_sent: f64 = response
        .headers()
        .get("content-length")
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);

    log_request(
        env,
        &RequestEvent {
            account_id: account.unwrap_or(""),
            product_id: product.unwrap_or(""),
            file_path: key.unwrap_or(""),
            method: method.as_str(),
            user_id: header_str(headers, "x-source-user-id"),
            country: header_str(headers, "cf-ipcountry"),
            content_type: &content_type,
            bytes_sent,
            status_code: response.status() as f64,
        },
    );
}

// ── Location broadcasting ──────────────────────────────────────────

/// Properties extracted from the Cloudflare `request.cf` object.
#[derive(Default)]
struct CfProperties {
    latitude: String,
    longitude: String,
    city: String,
    colo: String,
}

impl CfProperties {
    fn from_request(req: &web_sys::Request) -> Self {
        let cf =
            js_sys::Reflect::get(req, &wasm_bindgen::JsValue::from_str("cf")).unwrap_or_default();
        if cf.is_undefined() || cf.is_null() {
            return Self::default();
        }
        let get = |key: &str| -> String {
            js_sys::Reflect::get(&cf, &wasm_bindgen::JsValue::from_str(key))
                .ok()
                .map(|v| {
                    v.as_string()
                        .unwrap_or_else(|| v.as_f64().map(|n| n.to_string()).unwrap_or_default())
                })
                .unwrap_or_default()
        };
        Self {
            latitude: get("latitude"),
            longitude: get("longitude"),
            city: get("city"),
            colo: get("colo"),
        }
    }
}

struct LocationEvent {
    cf: CfProperties,
    country: String,
    account: String,
    product: String,
    key: String,
    api_base_url: String,
    api_auth: ApiAuth,
}

/// Broadcast the request's geolocation to WebSocket viewers via the public-log-stream service.
/// Runs entirely inside `wait_until` so it never blocks the response.
fn maybe_broadcast_location(ctx: &Context, env: &Env, event: LocationEvent) {
    let (Ok(lat), Ok(lon)) = (
        event.cf.latitude.parse::<f64>(),
        event.cf.longitude.parse::<f64>(),
    ) else {
        return;
    };

    let Ok(location_ws) = env.service("PUBLIC_LOG_STREAM") else {
        return;
    };

    ctx.wait_until(async move {
        let is_public = cache::get_or_fetch_product(
            &event.api_base_url,
            &event.account,
            &event.product,
            &event.api_auth,
            "",
            None,
        )
        .await
        .map(|p| p.is_public())
        .unwrap_or(false);
        if !is_public {
            return;
        }

        let body = serde_json::json!({
            "lat": lat,
            "lon": lon,
            "city": event.cf.city,
            "country": event.country,
            "colo": event.cf.colo,
            "account_id": event.account,
            "product_id": event.product,
            "path": event.key,
        });
        let mut init = worker::RequestInit::new();
        init.with_method(worker::Method::Post);
        init.with_body(Some(wasm_bindgen::JsValue::from_str(&body.to_string())));
        let _ = location_ws
            .fetch("https://public-log-stream/location", Some(init))
            .await;
    });
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
