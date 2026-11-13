//! A request's W3C trace context (ADR-016).
//!
//! The trace comes from the request's `traceparent` header when it carries a
//! well-formed one, as source.coop's server does, and is started here
//! otherwise. The proxy returns its ID as `x-source-trace-id`, writes it on
//! the line it logs for a 5xx, and sends a child `traceparent` with every
//! Source API call, so the API continues the same trace.
//!
//! Randomness is passed in rather than drawn here, so this module builds and
//! is tested natively (`tests/trace.rs`).

/// The trace a request belongs to.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceContext {
    /// 32 lowercase hex digits.
    pub trace_id: String,
    /// Whether the caller sampled the trace. Passed on unchanged, so the API
    /// records spans only for traces its caller records; a trace started here
    /// is unsampled.
    pub sampled: bool,
}

impl TraceContext {
    /// The trace `traceparent` names, or a new unsampled one with an ID drawn
    /// from `new_trace_id` (called only then) when the header is absent or
    /// malformed. An untrusted header chooses only the ID, which nothing
    /// relies on for access, and whether the API records the trace.
    pub fn from_traceparent(
        traceparent: Option<&str>,
        new_trace_id: impl FnOnce() -> [u8; 16],
    ) -> Self {
        traceparent.and_then(parse).unwrap_or_else(|| Self {
            trace_id: hex(&new_trace_id()),
            sampled: false,
        })
    }

    /// A `traceparent` for a call made on this request's behalf, with
    /// `span_id` as its parent span.
    pub fn child(&self, span_id: [u8; 8]) -> String {
        let flags = if self.sampled { "01" } else { "00" };
        format!("00-{}-{}-{flags}", self.trace_id, hex(&span_id))
    }
}

/// W3C Trace Context level 1: `version-traceid-parentid-flags`. Version `ff`
/// is invalid; version `00` has exactly four fields, and a later version may
/// append more, which are ignored.
fn parse(header: &str) -> Option<TraceContext> {
    let mut fields = header.trim().split('-');
    let version = fields.next()?;
    let trace_id = fields.next()?;
    let parent_id = fields.next()?;
    let flags = fields.next()?;
    if !is_hex(version, 2) || version == "ff" || (version == "00" && fields.next().is_some()) {
        return None;
    }
    if !is_hex(trace_id, 32) || !is_hex(parent_id, 16) || !is_hex(flags, 2) {
        return None;
    }
    if is_zero(trace_id) || is_zero(parent_id) {
        return None;
    }
    let flags = u8::from_str_radix(flags, 16).ok()?;
    Some(TraceContext {
        trace_id: trace_id.to_string(),
        sampled: flags & 1 == 1,
    })
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_zero(s: &str) -> bool {
    s.bytes().all(|b| b == b'0')
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
