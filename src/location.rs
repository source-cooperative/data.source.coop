//! Live location broadcasting for the public activity globe.
//!
//! Public product reads are batched per isolate and posted to the
//! public-log-stream service at most once per [`WINDOW_MS`]. An isolate runs in
//! a single datacenter, so batching in the isolate is batching by datacenter:
//! a busy colo collapses into one message carrying exact counts, while a quiet
//! colo still reports its first request. Nothing is sampled away; only the
//! number of posts shrinks (each post is a billed Durable Object request plus
//! the CPU of both Workers). Runs inside `wait_until`, so it never blocks the
//! response.

use std::cell::RefCell;
use std::time::Duration;

use worker::{Context, Delay, Env};

use crate::location_batch::{record, Pending, WINDOW_MS};
use crate::source_api::{self, ApiAuth};

/// Properties extracted from the Cloudflare `request.cf` object.
#[derive(Default)]
pub(crate) struct CfProperties {
    colo: String,
}

impl CfProperties {
    pub(crate) fn from_request(req: &web_sys::Request) -> Self {
        let cf =
            js_sys::Reflect::get(req, &wasm_bindgen::JsValue::from_str("cf")).unwrap_or_default();
        if cf.is_undefined() || cf.is_null() {
            return Self::default();
        }
        let colo = js_sys::Reflect::get(&cf, &wasm_bindgen::JsValue::from_str("colo"))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        Self { colo }
    }
}

pub(crate) struct LocationEvent {
    pub cf: CfProperties,
    pub account: String,
    pub product: String,
    pub api_base_url: String,
    pub api_auth: ApiAuth,
}

thread_local! {
    // Workers are single-threaded; this is the isolate's current batch.
    static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) };
}

/// Count this request toward the live globe. Only public products are counted.
pub(crate) fn maybe_broadcast_location(ctx: &Context, env: &Env, event: LocationEvent) {
    if event.cf.colo.is_empty() {
        return;
    }

    let Ok(location_ws) = env.service("PUBLIC_LOG_STREAM") else {
        return;
    };

    ctx.wait_until(async move {
        let is_public = source_api::cache::get_or_fetch_product(
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

        let product = format!("{}/{}", event.account, event.product);
        let leader = PENDING.with(|s| {
            record(
                &mut s.borrow_mut(),
                &event.cf.colo,
                product,
                js_sys::Date::now(),
            )
        });
        if !leader {
            return;
        }

        Delay::from(Duration::from_millis(WINDOW_MS as u64)).await;
        let Some(batch) = PENDING.with(|s| s.borrow_mut().take()) else {
            return;
        };

        let body = serde_json::json!({ "colo": batch.colo, "products": batch.products });
        let mut init = worker::RequestInit::new();
        init.with_method(worker::Method::Post);
        init.with_body(Some(wasm_bindgen::JsValue::from_str(&body.to_string())));
        let _ = location_ws
            .fetch("https://public-log-stream/location", Some(init))
            .await;
    });
}
