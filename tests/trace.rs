//! Native unit tests for the request trace context (`trace`), included via
//! `#[path]` like `tests/keys.rs`.

#[path = "../src/trace.rs"]
mod trace;

use trace::TraceContext;

const NEW_ID: [u8; 16] = [0xab; 16];
const SPAN: [u8; 8] = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];

fn from(header: &str) -> TraceContext {
    TraceContext::from_traceparent(Some(header), || NEW_ID)
}

fn started_here(t: &TraceContext) -> bool {
    t.trace_id == "abababababababababababababababab" && !t.sampled
}

#[test]
fn a_well_formed_traceparent_names_the_trace() {
    let t = from("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01");
    assert_eq!(t.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
    assert!(t.sampled);
    assert!(!from("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00").sampled);
}

#[test]
fn a_later_version_may_carry_more_fields() {
    let t = from("01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra");
    assert_eq!(t.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
}

#[test]
fn anything_else_starts_a_new_unsampled_trace() {
    assert!(started_here(&TraceContext::from_traceparent(None, || {
        NEW_ID
    })));
    for bad in [
        "",
        "garbage",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
        "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
        "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
        "00-4bf92f3577b34da6a3ce929d0e0e473-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-zz",
    ] {
        assert!(started_here(&from(bad)), "{bad:?} must not be accepted");
    }
}

#[test]
fn a_child_keeps_the_trace_and_its_sampling() {
    let sampled = from("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01");
    assert_eq!(
        sampled.child(SPAN),
        "00-4bf92f3577b34da6a3ce929d0e0e4736-0102030405060708-01"
    );
    let unsampled = TraceContext::from_traceparent(None, || NEW_ID);
    assert_eq!(
        unsampled.child(SPAN),
        "00-abababababababababababababababab-0102030405060708-00"
    );
}
