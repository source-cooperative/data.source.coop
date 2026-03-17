mod fetch_connector;
mod noop_creds;
mod registry;
mod routing;
mod worker_backend;
mod worker_infra;

use multistore::proxy::{GatewayResponse, ProxyGateway};
use multistore::route_handler::RequestInfo;
use noop_creds::NoopCredentialRegistry;
use registry::SourceCoopRegistry;
use routing::{parse_request, ParsedRequest};
use worker::*;
use worker_backend::WorkerBackend;
use worker_infra::*;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[event(fetch)]
async fn fetch(req: web_sys::Request, env: Env, _ctx: Context) -> Result<web_sys::Response> {
    console_error_panic_hook::set_once();

    let api_base_url = env
        .var("SOURCE_API_URL")
        .map(|v| v.to_string())
        .unwrap_or_else(|_| "https://source.coop".to_string());

    let registry = SourceCoopRegistry::new(api_base_url);
    let creds = NoopCredentialRegistry;

    let gateway = ProxyGateway::new(
        WorkerBackend,
        registry.clone(),
        creds,
        WorkerForwarder,
        None,
    );

    // Extract body stream BEFORE any wrapping — no lock, zero-cost ref.
    let js_body = JsBody(req.body());

    // Parse request metadata from the raw web_sys::Request.
    let method: http::Method = req.method().parse().unwrap_or(http::Method::GET);
    let url_str = req.url();
    let uri: http::Uri = url_str
        .parse()
        .unwrap_or_else(|_| http::Uri::from_static("/"));
    let path = uri.path().to_string();
    let query = uri.query().map(|q| q.to_string());
    let headers = convert_ws_headers(&req.headers());

    // Handle OPTIONS preflight
    if method == http::Method::OPTIONS {
        return Ok(add_cors(ws_error_response(204, "")));
    }

    let response = match parse_request(&method, &path, query.as_deref()) {
        ParsedRequest::Index => {
            ws_error_response(200, &format!("Source Cooperative Data Proxy v{}", VERSION))
        }

        ParsedRequest::WriteNotAllowed => ws_error_response(405, "Method Not Allowed"),

        ParsedRequest::BadRequest(msg) => ws_error_response(400, &msg),

        ParsedRequest::AccountList { account, .. } => {
            match registry.list_products(&account).await {
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
                Err(_) => ws_xml_response(
                    200,
                    &format!(
                        r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>{}</Name><Prefix></Prefix><IsTruncated>false</IsTruncated></ListBucketResult>"#,
                        account
                    ),
                ),
            }
        }

        ParsedRequest::ObjectRequest {
            rewritten_path,
            query: q,
        } => {
            let req_info = RequestInfo::new(&method, &rewritten_path, q.as_deref(), &headers, None);
            match gateway
                .handle_request(&req_info, js_body, collect_js_body)
                .await
            {
                GatewayResponse::Response(result) => proxy_result_to_ws_response(result),
                GatewayResponse::Forward(resp) => forward_response_to_ws(resp),
            }
        }

        ParsedRequest::ProductList {
            rewritten_path,
            query: q,
        } => {
            let req_info = RequestInfo::new(&method, &rewritten_path, Some(&q), &headers, None);
            match gateway
                .handle_request(&req_info, js_body, collect_js_body)
                .await
            {
                GatewayResponse::Response(result) => proxy_result_to_ws_response(result),
                GatewayResponse::Forward(resp) => forward_response_to_ws(resp),
            }
        }
    };

    Ok(add_cors(response))
}

/// Add CORS headers to a response.
fn add_cors(resp: web_sys::Response) -> web_sys::Response {
    let h = resp.headers();
    let _ = h.set("access-control-allow-origin", "*");
    let _ = h.set("access-control-allow-methods", "GET, HEAD, OPTIONS");
    let _ = h.set("access-control-allow-headers", "*");
    let _ = h.set("access-control-expose-headers", "*");
    resp
}
