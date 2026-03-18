//! A lightweight `tracing` layer that routes log output to the Workers
//! `console.log` / `console.error` / `console.warn` APIs.
//!
//! This avoids pulling in `tracing-subscriber` (which depends on `std::time`
//! and other things unavailable on `wasm32`). Instead we implement a minimal
//! [`tracing::Subscriber`] that formats events and forwards them through
//! `worker::console_log!` / `console_error!` / `console_warn!`.

use tracing::field::{Field, Visit};
use tracing::span;
use tracing::{Event, Level, Metadata, Subscriber};

/// A minimal tracing subscriber that logs to the Workers console.
///
/// Install once at the start of each request:
/// ```rust,ignore
/// tracing::subscriber::set_global_default(WorkerSubscriber::new())
///     .ok(); // ignore if already set
/// ```
pub struct WorkerSubscriber {
    max_level: Level,
}

impl WorkerSubscriber {
    pub fn new() -> Self {
        Self {
            max_level: Level::DEBUG,
        }
    }

    #[allow(dead_code)]
    pub fn with_max_level(mut self, level: Level) -> Self {
        self.max_level = level;
        self
    }
}

impl Subscriber for WorkerSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= &self.max_level
    }

    fn new_span(&self, _attrs: &span::Attributes<'_>) -> span::Id {
        // We don't track spans — just log events.
        span::Id::from_u64(1)
    }

    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}
    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}
    fn event(&self, event: &Event<'_>) {
        let metadata = event.metadata();
        let level = metadata.level();
        let target = metadata.target();

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let msg = if visitor.fields.is_empty() {
            format!("[{level}] {target}: {}", visitor.message)
        } else {
            format!(
                "[{level}] {target}: {} {{ {} }}",
                visitor.message,
                visitor.fields.join(", ")
            )
        };

        // Route to the appropriate console method based on level.
        // worker::console_log! and friends are macros that call into JS.
        match *level {
            Level::ERROR => worker::console_error!("{}", msg),
            Level::WARN => worker::console_warn!("{}", msg),
            _ => worker::console_log!("{}", msg),
        }
    }

    fn enter(&self, _span: &span::Id) {}
    fn exit(&self, _span: &span::Id) {}
}

/// Visitor that extracts the `message` field and collects remaining fields
/// into `key=value` pairs.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: Vec<String>,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
        } else {
            self.fields.push(format!("{}={:?}", field.name(), value));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push(format!("{}=\"{}\"", field.name(), value));
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.push(format!("{}={}", field.name(), value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.push(format!("{}={}", field.name(), value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.push(format!("{}={}", field.name(), value));
    }
}
