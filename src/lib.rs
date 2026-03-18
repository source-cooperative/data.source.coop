mod cache;
mod registry;
pub mod routing;

use multistore::proxy::{GatewayResponse, ProxyGateway};
use multistore::route_handler::RequestInfo;
use multistore_cf_workers::{
    collect_js_body, convert_ws_headers, forward_response_to_ws, proxy_result_to_ws_response,
    ws_error_response, ws_xml_response, JsBody, NoopCredentialRegistry, WorkerBackend,
    WorkerSubscriber,
};
use multistore_path_mapping::{MappedRegistry, PathMapping};
use registry::SourceCoopRegistry;
use routing::{classify_request, RequestClass};
use worker::*;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Source Cooperative path mapping: `/{account}/{product}/{key}`
/// → internal bucket `account--product`, display name shows just `account`.
fn source_coop_mapping() -> PathMapping {
    PathMapping {
        bucket_segments: 2,
        bucket_separator: "--".to_string(),
        display_bucket_segments: 1,
    }
}

#[event(fetch)]
async fn fetch(req: web_sys::Request, env: Env, _ctx: Context) -> Result<web_sys::Response> {
    console_error_panic_hook::set_once();

    // ── Tracing ────────────────────────────────────────────────────
    let log_level = env
        .var("LOG_LEVEL")
        .map(|v| v.to_string())
        .unwrap_or_else(|_| "WARN".to_string());
    let max_level = match log_level.to_uppercase().as_str() {
        "TRACE" => tracing::Level::TRACE,
        "DEBUG" => tracing::Level::DEBUG,
        "INFO" => tracing::Level::INFO,
        "ERROR" => tracing::Level::ERROR,
        _ => tracing::Level::WARN,
    };
    tracing::subscriber::set_global_default(WorkerSubscriber::new().with_max_level(max_level)).ok();

    // ── Configuration ──────────────────────────────────────────────
    let api_base_url = env
        .var("SOURCE_API_URL")
        .map(|v| v.to_string())
        .unwrap_or_else(|_| "https://source.coop".to_string());

    let mapping = source_coop_mapping();
    let registry = SourceCoopRegistry::new(api_base_url);
    let mapped_registry = MappedRegistry::new(registry.clone(), mapping.clone());

    let gateway = ProxyGateway::new(
        WorkerBackend,
        mapped_registry,
        NoopCredentialRegistry,
        None,
    );

    // ── Parse request ──────────────────────────────────────────────
    let js_body = JsBody(req.body());
    let method: http::Method = req.method().parse().unwrap_or(http::Method::GET);
    let url_str = req.url();
    let uri: http::Uri = url_str
        .parse()
        .unwrap_or_else(|_| http::Uri::from_static("/"));
    let path = percent_encoding::percent_decode_str(uri.path())
        .decode_utf8_lossy()
        .to_string();
    let query = uri.query().map(|q| q.to_string());
    let mut headers = convert_ws_headers(&req.headers());

    // Strip AWS auth headers — this proxy is anonymous-only.
    headers.remove(http::header::AUTHORIZATION);
    headers.remove("x-amz-security-token");
    headers.remove("x-amz-content-sha256");

    // ── OPTIONS preflight ──────────────────────────────────────────
    if method == http::Method::OPTIONS {
        return Ok(add_cors(ws_error_response(204, "")));
    }

    // ── Reject write methods ───────────────────────────────────────
    if matches!(
        method,
        http::Method::PUT | http::Method::POST | http::Method::DELETE | http::Method::PATCH
    ) {
        return Ok(add_cors(ws_error_response(405, "Method Not Allowed")));
    }

    tracing::debug!("{} {}", method, path);

    let response = match classify_request(&mapping, &path, query.as_deref()) {
        RequestClass::Index => {
            ws_error_response(200, &format!("Source Cooperative Data Proxy v{}", VERSION))
        }

        RequestClass::BadRequest(msg) => ws_error_response(400, &msg),

        RequestClass::AccountList { account } => {
            handle_account_list(&registry, &account).await
        }

        RequestClass::ProxyRequest {
            rewritten_path,
            query: q,
        } => {
            let req_info =
                RequestInfo::new(&method, &rewritten_path, q.as_deref(), &headers, None);
            dispatch_to_gateway(&gateway, &req_info, js_body, &rewritten_path).await
        }
    };

    Ok(add_cors(response))
}

// ── Request classification (see routing.rs) ───────────────────────

// ── Account listing ────────────────────────────────────────────────

/// Handle `GET /{account}?list-type=2` — list products via the Source Coop API.
async fn handle_account_list(
    registry: &SourceCoopRegistry,
    account: &str,
) -> web_sys::Response {
    match registry.list_products(account).await {
        Ok(products) => {
            let prefixes_xml: String = products
                .iter()
                .map(|p| {
                    format!("<CommonPrefixes><Prefix>{}/</Prefix></CommonPrefixes>", p)
                })
                .collect();
            let xml = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>{}</Name><Prefix></Prefix><Delimiter>/</Delimiter><IsTruncated>false</IsTruncated>{}</ListBucketResult>"#,
                account, prefixes_xml
            );
            ws_xml_response(200, &xml)
        }
        Err(e) => {
            tracing::error!("AccountList({}) error: {:?}", account, e);
            ws_xml_response(
                200,
                &format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>{}</Name><Prefix></Prefix><IsTruncated>false</IsTruncated></ListBucketResult>"#,
                    account
                ),
            )
        }
    }
}

// ── Gateway dispatch ───────────────────────────────────────────────

/// Dispatch a request through the ProxyGateway and convert the result to a web_sys::Response.
async fn dispatch_to_gateway(
    gateway: &ProxyGateway<
        WorkerBackend,
        MappedRegistry<SourceCoopRegistry>,
        NoopCredentialRegistry,
    >,
    req_info: &RequestInfo<'_>,
    js_body: JsBody,
    rewritten_path: &str,
) -> web_sys::Response {
    let result = gateway
        .handle_request(req_info, js_body, collect_js_body)
        .await;

    match &result {
        GatewayResponse::Response(ref r) if r.status >= 400 => {
            let body_str = match &r.body {
                multistore::route_handler::ProxyResponseBody::Bytes(b) => {
                    std::str::from_utf8(b).unwrap_or("<binary>").to_string()
                }
                multistore::route_handler::ProxyResponseBody::Empty => "<empty>".to_string(),
            };
            if r.status >= 500 {
                tracing::error!("{} returned {}: {}", rewritten_path, r.status, body_str);
            } else {
                tracing::warn!("{} returned {}: {}", rewritten_path, r.status, body_str);
            }
        }
        GatewayResponse::Forward(ref r) if r.status >= 400 => {
            if r.status >= 500 {
                tracing::error!("{} forwarded {}", rewritten_path, r.status);
            } else {
                tracing::warn!("{} forwarded {}", rewritten_path, r.status);
            }
        }
        _ => {}
    }

    match result {
        GatewayResponse::Response(result) => proxy_result_to_ws_response(result),
        GatewayResponse::Forward(resp) => forward_response_to_ws(resp),
    }
}


// ── CORS ───────────────────────────────────────────────────────────

/// Add CORS headers to a response.
fn add_cors(resp: web_sys::Response) -> web_sys::Response {
    let h = resp.headers();
    let _ = h.set("access-control-allow-origin", "*");
    let _ = h.set("access-control-allow-methods", "GET, HEAD, OPTIONS");
    let _ = h.set("access-control-allow-headers", "*");
    let _ = h.set("access-control-expose-headers", "*");
    resp
}
