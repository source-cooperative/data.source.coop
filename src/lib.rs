mod analytics;
mod cache;
mod handlers;
mod pagination;
mod registry;

use analytics::{extract_path_segments, log_request, RequestEvent};
use handlers::{AccountListHandler, IndexHandler};
use multistore::api::response::ErrorResponse;
use multistore::proxy::ProxyGateway;
use multistore::route_handler::RequestInfo;
use multistore::router::Router;
use multistore_cf_workers::{
    collect_js_body, error_response, headermap_from_js, response_from_gateway, xml_response,
    JsBody, NoopCredentialRegistry, WorkerBackend, WorkerSubscriber,
};
use multistore_path_mapping::{MappedRegistry, PathMapping};
use registry::SourceCoopRegistry;
use worker::{event, Context, Env, Result};

/// Separator used to join account + product into a single internal bucket name.
pub(crate) const BUCKET_SEPARATOR: &str = ":";

#[event(fetch)]
async fn fetch(req: web_sys::Request, env: Env, _ctx: Context) -> Result<web_sys::Response> {
    console_error_panic_hook::set_once();
    let max_level = init_tracing(&env);
    let config = load_config(&env);

    // ── Parse request ──────────────────────────────────────────────
    let js_body = JsBody(req.body());
    let method: http::Method = req.method().parse().unwrap_or(http::Method::GET);
    let uri: http::Uri = req
        .url()
        .parse()
        .unwrap_or_else(|_| http::Uri::from_static("/"));
    let path = percent_encoding::percent_decode_str(uri.path())
        .decode_utf8_lossy()
        .to_string();
    let query = uri.query().map(|q| q.to_string());
    let mut headers = headermap_from_js(&req.headers());

    // Strip AWS auth headers — this proxy is anonymous-only.
    headers.remove(http::header::AUTHORIZATION);
    headers.remove("x-amz-security-token");
    headers.remove("x-amz-content-sha256");

    let request_id = extract_request_id(&headers);

    // ── Short-circuit: OPTIONS preflight ────────────────────────────
    if method == http::Method::OPTIONS {
        return Ok(add_cors(error_response(204, "")));
    }

    // ── Short-circuit: write methods ───────────────────────────────
    if is_write_method(&method) {
        let resp = ErrorResponse {
            code: "MethodNotAllowed".to_string(),
            message: "Method Not Allowed".to_string(),
            resource: String::new(),
            request_id: request_id.to_string(),
        };
        return Ok(add_cors(xml_response(405, &resp.to_xml())));
    }

    // ── Path rewriting ─────────────────────────────────────────────
    // Source Cooperative path mapping: `/{account}/{product}/{key}`
    // → internal bucket `account:product`, display name shows just `account`.
    let mapping = PathMapping {
        bucket_segments: 2,
        bucket_separator: BUCKET_SEPARATOR.to_string(),
        display_bucket_segments: 1,
    };
    let (rewritten_path, rewritten_query) = mapping.rewrite_request(&path, query.as_deref());

    // ── Build gateway with route handlers ──────────────────────────
    let registry = SourceCoopRegistry::new(
        config.api_base_url.clone(),
        config.api_secret.clone(),
        request_id.clone(),
    );

    let gateway = ProxyGateway::new(
        WorkerBackend,
        MappedRegistry::new(registry.clone(), mapping.clone()),
        NoopCredentialRegistry,
        None,
    )
    .with_router(
        Router::new()
            .route("/", IndexHandler)
            .route("/{bucket}", AccountListHandler::new(registry, &mapping)),
    )
    .with_debug_errors(max_level >= tracing::Level::DEBUG);

    // ── Dispatch through gateway ──────────────────────────────────
    let span = tracing::info_span!("request", %request_id, %method, %path);
    let _guard = span.enter();

    let req_info = RequestInfo::new(
        &method,
        &rewritten_path,
        rewritten_query.as_deref(),
        &headers,
        None,
    );
    let result = gateway
        .handle_request(&req_info, js_body, collect_js_body)
        .await;
    let response = response_from_gateway(result);
    tracing::info!(status = response.status(), "response");

    // ── Extract path segments (used by analytics + location broadcast) ──
    let (account, product, key) = extract_path_segments(&path);

    // ── Analytics ───────────────────────────────────────────────
    log_analytics(&env, &headers, &response, &method, account, product, key);

    let response = add_cors(response);
    if !request_id.is_empty() {
        let _ = response.headers().set("x-request-id", &request_id);
    }
    Ok(response)
}

// ── Helpers ─────────────────────────────────────────────────────────

struct AppConfig {
    api_base_url: String,
    api_secret: Option<String>,
}

fn load_config(env: &Env) -> AppConfig {
    AppConfig {
        api_base_url: env
            .var("SOURCE_API_URL")
            .map(|v| v.to_string())
            .unwrap_or_else(|_| "https://source.coop".to_string()),
        api_secret: env.secret("SOURCE_API_SECRET").map(|v| v.to_string()).ok(),
    }
}

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

fn is_write_method(method: &http::Method) -> bool {
    matches!(
        *method,
        http::Method::PUT | http::Method::POST | http::Method::DELETE | http::Method::PATCH
    )
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

// ── CORS ────────────────────────────────────────────────────────────

fn add_cors(resp: web_sys::Response) -> web_sys::Response {
    let h = resp.headers();
    for (name, value) in [
        ("access-control-allow-origin", "*"),
        ("access-control-allow-methods", "GET, HEAD, OPTIONS"),
        ("access-control-allow-headers", "*"),
        ("access-control-expose-headers", "*"),
    ] {
        if let Err(e) = h.set(name, value) {
            tracing::warn!("failed to set CORS header {}: {:?}", name, e);
        }
    }
    resp
}
