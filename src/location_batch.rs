//! The wasm-free half of live location broadcasting: how an isolate gathers
//! public reads into one batch and decides who flushes it. Kept apart from
//! `location.rs` so it is covered by native tests (the lib itself is `cdylib`
//! with `test = false`).

use std::collections::HashMap;

/// How long an isolate gathers requests before posting them as one batch. The
/// globe ticks every 500 ms, so this only bounds how stale a datacenter's
/// activity can be when it appears.
pub const WINDOW_MS: f64 = 3000.0;

/// Requests gathered since the batch opened.
pub struct Pending {
    pub colo: String,
    pub opened_at: f64,
    pub products: HashMap<String, u32>,
}

/// Add one request to the batch. Returns true when the caller must flush it
/// after the window: it opened the batch, or the leader that did is long gone
/// (an evicted `wait_until` would otherwise strand the batch forever).
pub fn record(slot: &mut Option<Pending>, colo: &str, product: String, now: f64) -> bool {
    let stale = slot
        .as_ref()
        .is_some_and(|p| now - p.opened_at > 2.0 * WINDOW_MS);
    let leader = slot.is_none() || stale;
    let p = slot.get_or_insert_with(|| Pending {
        colo: colo.to_string(),
        opened_at: now,
        products: HashMap::new(),
    });
    if stale {
        p.opened_at = now;
    }
    *p.products.entry(product).or_default() += 1;
    leader
}
