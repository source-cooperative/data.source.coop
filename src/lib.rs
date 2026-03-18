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
    let mut headers = convert_ws_headers(&req.headers());

    // Strip AWS auth headers — this proxy is anonymous-only, and forwarding
    // them causes multistore to reject the request with AccessDenied when
    // the credential registry has no matching key.
    headers.remove(http::header::AUTHORIZATION);
    headers.remove("x-amz-security-token");
    headers.remove("x-amz-content-sha256");

    // Handle OPTIONS preflight
    if method == http::Method::OPTIONS {
        return Ok(add_cors(ws_error_response(204, "")));
    }

    let parsed = parse_request(&method, &path, query.as_deref());
    worker::console_log!(
        "{} {} -> {:?}",
        method,
        path,
        match &parsed {
            ParsedRequest::Index => "Index".to_string(),
            ParsedRequest::WriteNotAllowed => "WriteNotAllowed".to_string(),
            ParsedRequest::BadRequest(msg) => format!("BadRequest({})", msg),
            ParsedRequest::AccountList { account, .. } => format!("AccountList({})", account),
            ParsedRequest::ObjectRequest { rewritten_path, .. } =>
                format!("ObjectRequest({})", rewritten_path),
            ParsedRequest::ProductList { rewritten_path, prefix_route, .. } =>
                format!(
                    "ProductList({}, prefix_route={})",
                    rewritten_path,
                    prefix_route.as_ref().map_or("none".to_string(), |r| {
                        format!("{}/{}", r.account, r.product)
                    })
                ),
        }
    );

    let response = match parsed {
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
                Err(e) => {
                    worker::console_error!("AccountList({}) error: {:?}", account, e);
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

        ParsedRequest::ObjectRequest {
            rewritten_path,
            query: q,
        } => {
            let req_info = RequestInfo::new(&method, &rewritten_path, q.as_deref(), &headers, None);
            let result = gateway
                .handle_request(&req_info, js_body, collect_js_body)
                .await;
            match result {
                GatewayResponse::Response(ref r) => {
                    if r.status >= 400 {
                        let body_str = match &r.body {
                            multistore::route_handler::ProxyResponseBody::Bytes(b) =>
                                std::str::from_utf8(b).unwrap_or("<binary>").to_string(),
                            multistore::route_handler::ProxyResponseBody::Empty =>
                                "<empty>".to_string(),
                        };
                        worker::console_error!(
                            "ObjectRequest({}) returned {}: {}",
                            rewritten_path,
                            r.status,
                            body_str
                        );
                    }
                }
                GatewayResponse::Forward(ref r) => {
                    if r.status >= 400 {
                        worker::console_error!(
                            "ObjectRequest({}) forwarded {}",
                            rewritten_path,
                            r.status
                        );
                    }
                }
            }
            match result {
                GatewayResponse::Response(result) => proxy_result_to_ws_response(result),
                GatewayResponse::Forward(resp) => forward_response_to_ws(resp),
            }
        }

        ParsedRequest::ProductList {
            rewritten_path,
            query: q,
            prefix_route,
        } => {
            let req_info = RequestInfo::new(&method, &rewritten_path, Some(&q), &headers, None);
            let result = gateway
                .handle_request(&req_info, js_body, collect_js_body)
                .await;
            match result {
                GatewayResponse::Response(ref r) => {
                    if r.status >= 400 {
                        let body_str = match &r.body {
                            multistore::route_handler::ProxyResponseBody::Bytes(b) =>
                                std::str::from_utf8(b).unwrap_or("<binary>").to_string(),
                            multistore::route_handler::ProxyResponseBody::Empty =>
                                "<empty>".to_string(),
                        };
                        worker::console_error!(
                            "ProductList({}) returned {}: {}",
                            rewritten_path,
                            r.status,
                            body_str
                        );
                    }
                }
                GatewayResponse::Forward(ref r) => {
                    if r.status >= 400 {
                        worker::console_error!(
                            "ProductList({}) forwarded {}",
                            rewritten_path,
                            r.status
                        );
                    }
                }
            }
            match result {
                GatewayResponse::Response(mut result) => {
                    if let Some(ref info) = prefix_route {
                        let bucket_name = rewritten_path.trim_start_matches('/');
                        result.body = rewrite_list_xml(
                            result.body,
                            bucket_name,
                            info,
                        );
                    }
                    proxy_result_to_ws_response(result)
                }
                GatewayResponse::Forward(resp) => forward_response_to_ws(resp),
            }
        }
    };

    Ok(add_cors(response))
}

/// Rewrite list XML response so clients see the original account/prefix view
/// rather than multistore's internal `account--product` bucket structure.
///
/// Rewrites:
/// - `<Name>` → account name
/// - Top-level `<Prefix>` → original prefix
/// - `<Key>` values → prepend `product/`
/// - `<CommonPrefixes><Prefix>` values → prepend `product/`
fn rewrite_list_xml(
    body: multistore::route_handler::ProxyResponseBody,
    internal_bucket: &str,
    info: &routing::PrefixRouteInfo,
) -> multistore::route_handler::ProxyResponseBody {
    use bytes::Bytes;
    use multistore::route_handler::ProxyResponseBody;

    let ProxyResponseBody::Bytes(bytes) = body else {
        return body;
    };
    let Ok(xml) = std::str::from_utf8(&bytes) else {
        return ProxyResponseBody::Bytes(bytes);
    };

    // Replace <Name>account--product</Name> → <Name>account</Name>
    let xml = xml.replace(
        &format!("<Name>{}</Name>", internal_bucket),
        &format!("<Name>{}</Name>", info.account),
    );

    // Replace the top-level <Prefix.../> or <Prefix>...</Prefix> with the
    // original prefix. quick-xml serializes empty strings as self-closing
    // tags (<Prefix/>), so we must handle both forms.
    let xml = if let Some(name_end) = xml.find("</Name>") {
        let after_name = &xml[name_end..];
        if let Some(rel_pos) = after_name.find("<Prefix/>") {
            // Self-closing <Prefix/> (empty prefix from quick-xml)
            let start = name_end + rel_pos;
            let end = start + "<Prefix/>".len();
            format!(
                "{}<Prefix>{}</Prefix>{}",
                &xml[..start],
                info.original_prefix,
                &xml[end..]
            )
        } else if let Some(rel_pos) = after_name.find("<Prefix>") {
            // Regular <Prefix>...</Prefix>
            let start = name_end + rel_pos;
            let end = start
                + xml[start..].find("</Prefix>").unwrap_or(0)
                + "</Prefix>".len();
            format!(
                "{}<Prefix>{}</Prefix>{}",
                &xml[..start],
                info.original_prefix,
                &xml[end..]
            )
        } else {
            xml
        }
    } else {
        xml
    };

    let product_prefix = format!("{}/", info.product);

    // Prepend product/ to all <Key>...</Key> values
    let xml = xml.replace("<Key>", &format!("<Key>{}", product_prefix));

    // Fix backend directory marker: the backend stores a 0-byte key at the
    // prefix path (e.g. `cholmes/overture`) which doesn't get stripped by
    // multistore because it lacks the trailing `/`. After our prepend it
    // becomes `overture/cholmes/overture` — normalize to `overture/`.
    let xml = xml.replace(
        &format!(
            "<Key>{}{}/{}</Key>",
            product_prefix, info.account, info.product
        ),
        &format!("<Key>{}</Key>", product_prefix),
    );

    // Prepend product/ to <Prefix> values inside <CommonPrefixes>
    let xml = xml.replace(
        "<CommonPrefixes><Prefix>",
        &format!("<CommonPrefixes><Prefix>{}", product_prefix),
    );

    ProxyResponseBody::from_bytes(Bytes::from(xml))
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
