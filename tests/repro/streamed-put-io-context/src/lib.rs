//! Minimal reproducer for the streamed-PUT I/O-context failure.
//!
//! Isolates the shape of multistore's write path with no S3, no auth and no
//! registry, so the only variable left is *when* the inbound request body
//! stream is touched relative to an await.
//!
//! Two arms, selected by path:
//!
//! * `/before` — attach the inbound stream to the outbound request in this
//!   request's own I/O context, before any await. This is what
//!   `ProxyGateway::op_needs_buffered_body` arranges for the multipart control
//!   ops and batch delete.
//! * `/after` — await first (standing in for the registry lookup and the STS
//!   `AssumeRoleWithWebIdentity` exchange), *then* attach the stream. This is
//!   what `WorkerBackend::forward` does for `PutObject` and `UploadPart`, which
//!   the classifier explicitly excludes from the guard.
//!
//! On wasm32 the wasm-bindgen executor is a single queue shared by every
//! in-flight request in the isolate, so a future parked on an await can be
//! polled while workerd's current I/O context belongs to a *different* request.
//! Touching this request's body stream at that moment is I/O on behalf of
//! someone else. Drive both arms concurrently and compare.

use worker::{event, Context, Env, Result};

#[event(fetch)]
async fn fetch(req: web_sys::Request, env: Env, _ctx: Context) -> Result<web_sys::Response> {
    console_error_panic_hook::set_once();

    let uri: http::Uri = req
        .url()
        .parse()
        .map_err(|e| worker::Error::RustError(format!("url parse: {e}")))?;
    let arm = uri.path().trim_matches('/').to_string();

    let origin = env.var("ORIGIN_URL")?.to_string();
    let slow = env.var("SLOW_URL")?.to_string();

    // Capture the inbound stream up front, exactly as `RequestParts::from_web_sys`
    // does before the gateway is entered. Capturing is not yet I/O; using it is.
    let body = req.body();
    let len = req.headers().get("content-length").ok().flatten();

    let outcome = if arm == "before" {
        let forwarded = forward(&origin, body, len).await;
        let _ = slow_call(&slow).await;
        forwarded
    } else {
        slow_call(&slow).await?;
        forward(&origin, body, len).await
    };

    match outcome {
        Ok(status) => text(200, &format!("ok {status}")),
        Err(e) => {
            // The prod symptom is an uncaught exception (edge 520). Here the
            // worker crate surfaces it as an Err, so capture the message rather
            // than letting it escape — the text is the whole point.
            let msg = format!("{e}");
            worker::console_error!("[{}] forward failed: {}", arm, msg);
            text(599, &msg)
        }
    }
}

/// Attach `body` to a PUT at `origin`, mirroring `WorkerBackend::forward`,
/// including the `FixedLengthStream` wrapper and its dropped `pipe_to` promise.
async fn forward(
    origin: &str,
    body: Option<web_sys::ReadableStream>,
    len: Option<String>,
) -> Result<u16> {
    let init = web_sys::RequestInit::new();
    init.set_method("PUT");

    if let Some(stream) = body {
        match len.as_deref().and_then(|v| v.parse::<u64>().ok()) {
            Some(n) => {
                let fls = new_fixed_length_stream(n)
                    .map_err(|e| rust_err("FixedLengthStream init failed", e))?;
                let transform: &web_sys::TransformStream = fls.as_ref();
                let _ = stream.pipe_to(&transform.writable());
                init.set_body(&transform.readable());
            }
            None => init.set_body(&stream),
        }
    }

    let ws = web_sys::Request::new_with_str_and_init(origin, &init)
        .map_err(|e| rust_err("outbound request build failed", e))?;
    let resp = worker::Fetch::Request(ws.into()).send().await?;
    let resp: web_sys::Response = resp.into();
    Ok(resp.status())
}

/// Stand-in for the awaits the gateway performs between capturing the body and
/// forwarding it: the Source API product/connection lookup and the STS exchange.
async fn slow_call(url: &str) -> Result<()> {
    let init = web_sys::RequestInit::new();
    init.set_method("GET");
    let ws = web_sys::Request::new_with_str_and_init(url, &init)
        .map_err(|e| rust_err("slow request build failed", e))?;
    worker::Fetch::Request(ws.into()).send().await?;
    Ok(())
}

/// Parts can exceed `u32::MAX`, so fall back to the BigInt constructor.
fn new_fixed_length_stream(
    len: u64,
) -> std::result::Result<worker::worker_sys::FixedLengthStream, wasm_bindgen::JsValue> {
    if len <= u32::MAX as u64 {
        worker::worker_sys::FixedLengthStream::new(len as u32)
    } else {
        worker::worker_sys::FixedLengthStream::new_big_int(js_sys::BigInt::from(len))
    }
}

fn rust_err(context: &str, e: wasm_bindgen::JsValue) -> worker::Error {
    worker::Error::RustError(format!("{context}: {e:?}"))
}

fn text(status: u16, body: &str) -> Result<web_sys::Response> {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    web_sys::Response::new_with_opt_str_and_init(Some(body), &init)
        .map_err(|e| rust_err("response build failed", e))
}
