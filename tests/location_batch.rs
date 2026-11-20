//! Native unit tests for the wasm-free `location_batch` module, included via
//! `#[path]` (the lib itself is `cdylib` with `test = false`).

#[path = "../src/location_batch.rs"]
mod location_batch;

use location_batch::{record, WINDOW_MS};

#[test]
fn first_request_leads_and_the_rest_join_its_batch() {
    let mut slot = None;
    assert!(record(&mut slot, "EWR", "a/x".into(), 0.0));
    assert!(!record(&mut slot, "EWR", "a/x".into(), 100.0));
    assert!(!record(&mut slot, "EWR", "a/y".into(), 200.0));
    let p = slot.unwrap();
    assert_eq!(p.colo, "EWR");
    assert_eq!(p.products["a/x"], 2);
    assert_eq!(p.products["a/y"], 1);
}

#[test]
fn a_stranded_batch_gets_a_new_leader_and_keeps_its_counts() {
    let mut slot = None;
    record(&mut slot, "EWR", "a/x".into(), 0.0);
    // The leader's wait_until never ran; a later request takes over.
    assert!(record(
        &mut slot,
        "EWR",
        "a/x".into(),
        2.0 * WINDOW_MS + 1.0
    ));
    let p = slot.unwrap();
    assert_eq!(p.products["a/x"], 2);
    assert_eq!(p.opened_at, 2.0 * WINDOW_MS + 1.0);
}

#[test]
fn a_batch_inside_twice_the_window_is_not_stale() {
    let mut slot = None;
    record(&mut slot, "EWR", "a/x".into(), 0.0);
    assert!(!record(&mut slot, "EWR", "a/x".into(), 2.0 * WINDOW_MS));
}
