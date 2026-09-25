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
mod keys;
mod location;
mod object_path;
mod pagination;
mod platform;
mod source_api;
mod sts;

use crate::config::AppConfig;
use crate::source_api::{ApiAuth, SourceCoopRegistry};
use analytics::log_analytics;
use handlers::{AccountListHandler, IndexHandler};
use multistore::api::response::ErrorResponse;
use multistore::error::ProxyError;
use multistore::proxy::{GatewayResponse, ProxyGateway};
use multistore::route_handler::{ProxyResult, RequestInfo};
use multistore::router::Router;
use multistore::types::TemporaryCredentials;
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
use multistore_sts::{build_sts_error_response, build_sts_response, try_parse_sts_request};
use object_path::{extract_path_segments, is_keyless_write, mapped_copy_source};
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
    let (mut parts, mut js_body) = RequestParts::from_web_sys(&req)
        .map_err(|e| worker::Error::RustError(format!("invalid request: {e}")))?;

    // The router matches `/.sts` exactly; a trailing-slash variant would
    // otherwise fall through to bucket mapping and 404 confusingly.
    if parts.path == "/.sts/" {
        parts.path.pop();
    }

    // AWS SDKs send query-protocol operations — STS `AssumeRoleWithWebIdentity`
    // among them — as a form-encoded POST body with no query string at all, and
    // the STS route handler reads its parameters from `RequestInfo::form_body`.
    // Collect that body up front or SDK clients fall through to the S3 pipeline
    // unhandled. A no-op passthrough for every other request shape (and for a
    // form POST with a missing or oversized `Content-Length`, which is streamed
    // rather than buffered into WASM memory), and the returned body is rebuilt
    // from the collected bytes so a mislabeled S3 write — `Content-Type` is
    // client-controlled — still reaches the gateway with its payload intact.
    js_body = parts
        .absorb_form_body(js_body)
        .await
        .map_err(|e| worker::Error::RustError(format!("invalid request body: {e}")))?;

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

    // ── Short-circuit: write to a keyless path ──────────────────────
    // A keyless PUT/DELETE (e.g. `aws s3 cp f s3://account/product` with no
    // trailing slash) targets the product root, which has no object key.
    // Forwarding it makes the upstream reject the streaming upload with a
    // misleading "x-amz-content-sha256 header is invalid"; return an actionable
    // 400 instead so the caller sees the real cause. See `is_keyless_write`.
    if !is_special_path && is_keyless_write(&parts.method, &parts.path) {
        let resp = ErrorResponse {
            code: "InvalidRequest".to_string(),
            message: format!(
                "Missing object key: a {} must address an object at \
                 /{{account}}/{{product}}/{{key}}, not the product root.",
                parts.method.as_str()
            ),
            resource: parts.path.clone(),
            request_id: request_id.clone(),
        };
        return Ok(add_cors(
            GatewayResponse::Response(ProxyResult::xml(400, resp.to_xml())).into_web_sys(),
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

    // ── Short-circuit: API-key and platform-token exchanges ────────
    // An `sck_` key at `/.sts` is not a token: nothing verifies it here —
    // source.coop answers for it, by hash (ADR-013). A platform IdP's token
    // (GitHub Actions, say) is verified here, then acts as the account
    // `RoleArn` names only if that account trusts it (ADR-014). Both are
    // handled ahead of the STS route, which serves the person issuer alone.
    if parts.path == "/.sts" {
        if let Some(result) = api_key_exchange(config, &parts, &env, &api_auth, &request_id).await {
            return Ok(finish(result, &request_id));
        }
        if let Some(result) = platform_exchange(config, &parts, &api_auth, &request_id).await {
            return Ok(finish(result, &request_id));
        }
    }

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

    // SigV4's canonical URI is the percent-encoded path the client signed over.
    // `RequestParts` decodes the path for bucket/key routing, so recover the raw
    // encoded path from the request URL for signature verification — otherwise a
    // key with an escaped character (e.g. a space → `%20`) fails with
    // SignatureDoesNotMatch. `Uri::path()` returns the path un-decoded; fall back
    // to the decoded signing path if the URL somehow won't parse.
    let signing_path = req
        .url()
        .parse::<http::Uri>()
        .map(|u| u.path().to_string())
        .unwrap_or_else(|_| rewrite.signing_path.clone());

    // See `object_path::mapped_copy_source`: the copy source must be mapped
    // into the registry's namespace, without mutating the header the client
    // signed over.
    let copy_source = mapped_copy_source(&parts.headers, &mapping);

    let request_info = RequestInfo::new(
        &parts.method,
        &rewrite.path,
        rewrite.query.as_deref(),
        &parts.headers,
        None,
    )
    .with_signing_path(&signing_path)
    .with_signing_query(rewrite.signing_query.as_deref())
    .with_form_body(parts.form_body.as_deref())
    .with_copy_source(copy_source.as_deref());

    let start_ms = js_sys::Date::now();
    let response = gateway
        .handle_request(&request_info, js_body, collect_js_body)
        .await
        .into_web_sys();
    let duration_ms = js_sys::Date::now() - start_ms;
    let status = response.status();
    tracing::info!(status, duration_ms, "response");

    // Production runs at LOG_LEVEL=WARN, so the `info` line above — and the
    // `info_span` carrying method/path — are both silenced. That leaves a 5xx
    // with no worker-side record at all, which is how the 520s on large streamed
    // PUTs ended up undiagnosable: Cloudflare logs the URL and nothing else.
    // Re-emit at WARN, with the span fields inlined since the span itself is
    // disabled at this level.
    //
    // The response headers are the relayed *backend* headers: on a forwarded
    // request the gateway passes the upstream status through verbatim, and
    // `RESPONSE_HEADER_DENYLIST` strips only hop-by-hop, auth and proxy-routing
    // names — so `server` and the `x-amz-*` ids survive. That is what
    // distinguishes the three candidates for a relayed 520 that S3 itself never
    // emits: `server: AmazonS3` plus a request id means S3 answered and the
    // status is real; a `cf-ray` on the response means the subrequest's status
    // was minted inside Cloudflare's egress path; neither means the runtime
    // synthesised it with no upstream reply at all.
    if status >= 500 {
        let h = response.headers();
        let hv = |name: &str| h.get(name).ok().flatten().unwrap_or_default();
        tracing::warn!(
            status,
            duration_ms,
            method = %parts.method,
            path = %parts.path,
            content_length = header_str(&parts.headers, "content-length"),
            resp_server = %hv("server"),
            resp_cf_ray = %hv("cf-ray"),
            resp_amz_request_id = %hv("x-amz-request-id"),
            resp_amz_id_2 = %hv("x-amz-id-2"),
            resp_content_type = %hv("content-type"),
            resp_content_length = %hv("content-length"),
            "server error response"
        );
    }

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

// ── API keys ────────────────────────────────────────────────────────

/// A response answered before the gateway, with the CORS headers and the
/// request id every gateway response carries — as `x-request-id`, and as
/// `x-amzn-requestid`, the header AWS SDKs read it from.
fn finish((status, xml): (u16, String), request_id: &str) -> web_sys::Response {
    let response =
        add_cors(GatewayResponse::Response(ProxyResult::xml(status, xml)).into_web_sys());
    if !request_id.is_empty() {
        let _ = response.headers().set("x-request-id", request_id);
        let _ = response.headers().set("x-amzn-requestid", request_id);
    }
    response
}

/// The rate-limiter binding for API-key exchanges, keyed by client IP.
const KEY_EXCHANGE_LIMIT: &str = "KEY_EXCHANGE_LIMIT";

/// The API-key exchange, if this request is one: `None` when it is not an
/// `AssumeRoleWithWebIdentity` carrying an `sck_` key, so the STS route takes
/// it. A key is accepted from the form body only — Cloudflare logs the URL —
/// and the refusal for one in the query string says so, because that is the
/// one mistake a user can fix.
async fn api_key_exchange(
    config: &AppConfig,
    parts: &RequestParts,
    env: &Env,
    api_auth: &ApiAuth,
    request_id: &str,
) -> Option<(u16, String)> {
    if let Some(parsed) = try_parse_sts_request(parts.query.as_deref()) {
        let is_key = parsed
            .as_ref()
            .is_ok_and(|sts| keys::looks_like_api_key(&sts.web_identity_token));
        if !is_key {
            return None; // a token in the query string is the STS route's
        }
        tracing::warn!(%request_id, reason = "query_string", "API key exchange refused");
        return Some(key_refusal(
            "API key must be sent in the request body, not the URL",
            request_id,
        ));
    }
    let sts = try_parse_sts_request(parts.form_body.as_deref())?.ok()?;
    if !keys::looks_like_api_key(&sts.web_identity_token) {
        return None;
    }

    // Every attempt costs a lookup for a distinct key, so the flood to bound is
    // distinct junk keys from one place. Legitimate exchanges are rare — once
    // per session — so even a cluster behind one NAT stays well under the limit.
    let client_ip = header_str(&parts.headers, "cf-connecting-ip");
    if !within_rate_limit(env, client_ip).await {
        tracing::warn!(%request_id, reason = "rate_limited", "API key exchange refused");
        return Some((
            429,
            sts_error_xml(
                "Throttling",
                "too many API key exchanges from this address; retry later",
            ),
        ));
    }

    Some(
        match exchange_api_key(config, &sts, api_auth, request_id).await {
            Ok(creds) => build_sts_response(&creds),
            // One answer for every refusal of the key itself — unknown, revoked,
            // expired, disabled, malformed. `exchange_api_key` has logged why.
            Err(ProxyError::InvalidOidcToken(_)) => {
                key_refusal("API key was not accepted", request_id)
            }
            // A bad role is about the request, not the key; anything else is the
            // API being unreachable, which fails closed as a 500 the SDK retries.
            Err(e) => {
                tracing::warn!(%request_id, error = %e, "API key exchange failed");
                build_sts_error_response(&e)
            }
        },
    )
}

/// `InvalidIdentityToken`, with the request id in the message.
fn key_refusal(message: &str, request_id: &str) -> (u16, String) {
    build_sts_error_response(&ProxyError::InvalidOidcToken(with_request_id(
        message, request_id,
    )))
}

/// `message` with the request id, if there is one: SDKs show a user the
/// message and nothing else, and the id is what finds the log line.
fn with_request_id(message: &str, request_id: &str) -> String {
    if request_id.is_empty() {
        message.to_string()
    } else {
        format!("{message} (request id {request_id})")
    }
}

/// Hash the key, ask source.coop for its standing, and mint for the account
/// it names. A refused key is logged here, once, and returned as
/// `InvalidOidcToken`. The proxy knows only "malformed" or "inactive": which
/// of unknown, revoked, expired or disabled is in source.coop's log under the
/// same request id.
async fn exchange_api_key(
    config: &AppConfig,
    sts: &multistore_sts::request::StsRequest,
    api_auth: &ApiAuth,
    request_id: &str,
) -> Result<TemporaryCredentials, ProxyError> {
    let Some(key) = keys::parse_api_key(&sts.web_identity_token) else {
        tracing::warn!(%request_id, reason = "malformed", "API key exchange refused");
        return Err(ProxyError::InvalidOidcToken("malformed".into()));
    };
    let Some(role) = sts::role(
        &sts.role_arn,
        config.auth_issuer.clone(),
        config.auth_audiences.clone(),
        config.sts_max_session_duration_secs,
    ) else {
        return Err(ProxyError::RoleNotFound(sts.role_arn.clone()));
    };
    let key_hash = keys::key_hash(key);
    let standing = source_api::cache::get_or_fetch_key_standing(
        &config.api_base_url,
        &key_hash,
        api_auth,
        request_id,
    )
    .await
    .map_err(|e| match e {
        // The route answers 200 for any well-formed hash; a 404 means the API
        // does not serve it, which is a deployment mismatch, not an unknown key.
        ProxyError::BucketNotFound(_) => {
            ProxyError::Internal("key standing route not found".into())
        }
        e => e,
    })?;
    let key_id = standing.key_id.as_deref().unwrap_or("");
    let hash_prefix = &key_hash[..8];
    let account_id = match (standing.active, standing.account_id) {
        (true, Some(account_id)) => account_id,
        _ => {
            tracing::warn!(%request_id, key_id, hash_prefix, "API key is not active");
            return Err(ProxyError::InvalidOidcToken("inactive".into()));
        }
    };
    let creds = keys::credentials_for(
        &role,
        &account_id,
        sts.duration_seconds,
        &config.session_token_key,
    )?;
    tracing::info!(%request_id, key_id, %account_id, role = %role.role_id, "API key exchanged");
    Ok(creds)
}

/// Whether `client_ip` may make another exchange attempt now. A missing
/// binding is a deployment error, logged as such; it does not refuse traffic.
async fn within_rate_limit(env: &Env, client_ip: &str) -> bool {
    let key = if client_ip.is_empty() {
        "unknown"
    } else {
        client_ip
    };
    match env.rate_limiter(KEY_EXCHANGE_LIMIT) {
        Ok(limiter) => match limiter.limit(key.to_string()).await {
            Ok(outcome) => outcome.success,
            Err(e) => {
                tracing::warn!("rate limiter call failed: {e}");
                true
            }
        },
        Err(_) => {
            tracing::error!(
                "{KEY_EXCHANGE_LIMIT} binding is not configured; API-key exchanges are unlimited"
            );
            true
        }
    }
}

/// An STS-shaped error body with a code or message `build_sts_error_response`
/// does not produce.
fn sts_error_xml(code: &str, message: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ErrorResponse><Error><Code>{code}</Code><Message>{message}</Message></Error></ErrorResponse>"
    )
}

// ── Platform identity providers ─────────────────────────────────────

/// The exchange of a platform issuer's token, if this request carries one:
/// `None` for any other token, which the STS route takes. Parameters come from
/// the query string or the form body, never both, as at the STS route. Every
/// refusal of the account's trust reads the same, whether the account does not
/// exist or does not trust the token.
async fn platform_exchange(
    config: &AppConfig,
    parts: &RequestParts,
    api_auth: &ApiAuth,
    request_id: &str,
) -> Option<(u16, String)> {
    let sts = try_parse_sts_request(parts.query.as_deref())
        .or_else(|| try_parse_sts_request(parts.form_body.as_deref()))?
        .ok()?;
    let (header, claims) = platform::unverified(&sts.web_identity_token)?;
    let issuer = claims.get("iss")?.as_str()?;
    let audiences = config.platform_issuers.get(issuer)?;
    Some(
        match exchange_platform_token(
            config, &sts, &header, issuer, audiences, api_auth, request_id,
        )
        .await
        {
            Ok(creds) => build_sts_response(&creds),
            // `exchange_platform_token` has logged who asked to act as whom.
            Err(ProxyError::AccessDenied) => (
                403,
                sts_error_xml(
                    "AccessDenied",
                    &with_request_id(
                        "Not authorized to perform sts:AssumeRoleWithWebIdentity",
                        request_id,
                    ),
                ),
            ),
            Err(e) => {
                tracing::warn!(%request_id, %issuer, error = %e, "platform token exchange failed");
                build_sts_error_response(&e)
            }
        },
    )
}

/// Verify a platform issuer's token, then mint for the account `RoleArn`
/// names if that account trusts the token's issuer and subject (ADR-014). The
/// credentials act as the account, never as the token's subject. Everything
/// local comes first, so a token that fails it costs the Source API nothing.
async fn exchange_platform_token(
    config: &AppConfig,
    sts: &multistore_sts::request::StsRequest,
    header: &serde_json::Value,
    issuer: &str,
    audiences: &[String],
    api_auth: &ApiAuth,
    request_id: &str,
) -> Result<TemporaryCredentials, ProxyError> {
    let role = sts::role(
        &sts.role_arn,
        issuer.to_string(),
        audiences.to_vec(),
        config.sts_max_session_duration_secs,
    )
    .ok_or_else(|| ProxyError::RoleNotFound(sts.role_arn.clone()))?;
    // No angle brackets in the message: the STS error body carries it unescaped.
    let account = sts::account(&sts.role_arn).ok_or_else(|| {
        ProxyError::InvalidRequest(
            "RoleArn must name the account to act as: arn:aws:iam::ACCOUNT:role/ROLE".into(),
        )
    })?;
    let subject = platform::verify(
        &sts.web_identity_token,
        header,
        issuer,
        &role,
        &jwks_cache(),
    )
    .await?;
    source_api::cache::get_or_fetch_trust(
        &config.api_base_url,
        account,
        issuer,
        &subject,
        api_auth,
        request_id,
    )
    .await
    .map_err(|e| match e {
        ProxyError::AccessDenied => {
            tracing::warn!(%request_id, %issuer, %subject, %account, "account does not trust the token");
            e
        }
        // The route answers for any account; a 404 means the API does not
        // serve it, which is a deployment mismatch, not a refusal.
        ProxyError::BucketNotFound(_) => ProxyError::Internal("trusts route not found".into()),
        e => e,
    })?;
    let creds = keys::credentials_for(
        &role,
        account,
        sts.duration_seconds,
        &config.session_token_key,
    )?;
    tracing::info!(%request_id, %issuer, %subject, %account, role = %role.role_id, "platform token exchanged");
    Ok(creds)
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
