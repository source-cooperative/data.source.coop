//! Live location broadcasting: push each public product request's geolocation
//! to the public-log-stream WebSocket service. Fire-and-forget inside
//! `wait_until`, so it never blocks the response.

use worker::{Context, Env};

use crate::source_api::{self, ApiAuth};

/// Properties extracted from the Cloudflare `request.cf` object.
#[derive(Default)]
pub(crate) struct CfProperties {
    latitude: String,
    longitude: String,
    city: String,
    colo: String,
}

impl CfProperties {
    pub(crate) fn from_request(req: &web_sys::Request) -> Self {
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

pub(crate) struct LocationEvent {
    pub cf: CfProperties,
    pub country: String,
    pub account: String,
    pub product: String,
    pub key: String,
    pub api_base_url: String,
    pub api_auth: ApiAuth,
}

/// Broadcast the request's geolocation to WebSocket viewers via the public-log-stream service.
/// Runs entirely inside `wait_until` so it never blocks the response.
pub(crate) fn maybe_broadcast_location(ctx: &Context, env: &Env, event: LocationEvent) {
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
