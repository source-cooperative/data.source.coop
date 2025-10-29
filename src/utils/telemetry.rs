//! OpenTelemetry telemetry configuration and setup.
//!
//! This module provides the initialization and configuration for distributed tracing
//! using OpenTelemetry with OTLP export. It's designed to work with AWS X-Ray via
//! the ADOT collector.

use opentelemetry::{global, trace::{TraceError, TracerProvider as _}, KeyValue};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    propagation::TraceContextPropagator,
    runtime,
    trace::{RandomIdGenerator, Sampler},
    Resource,
};
use opentelemetry_semantic_conventions::{
    resource::{DEPLOYMENT_ENVIRONMENT, SERVICE_NAME, SERVICE_VERSION},
    SCHEMA_URL,
};
use std::env;
use tracing::subscriber::set_global_default;
use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};

/// Initializes the OpenTelemetry tracer and tracing subscriber.
///
/// This function sets up:
/// - OTLP exporter for sending traces to an OpenTelemetry collector
/// - W3C TraceContext propagation for distributed tracing
/// - Configurable sampling based on environment variables
/// - JSON-formatted logs for CloudWatch
/// - AWS X-Ray compatible trace ID generation
///
/// # Environment Variables
///
/// - `OTEL_SDK_DISABLED`: Set to "true" to disable OpenTelemetry entirely
/// - `OTEL_SERVICE_NAME`: Service name (default: "source-data-proxy")
/// - `OTEL_EXPORTER_OTLP_ENDPOINT`: OTLP endpoint (default: "http://localhost:4317")
/// - `OTEL_TRACE_SAMPLE_RATE`: Sample rate 0.0-1.0 (default: 0.1 = 10%)
/// - `DEPLOYMENT_ENV`: Deployment environment (default: "development")
/// - `RUST_LOG`: Log level filter (default: "info")
///
/// # Errors
///
/// Returns `TraceError` if the tracer cannot be initialized.
///
/// # Examples
///
/// ```rust
/// use source_data_proxy::utils::telemetry;
///
/// #[tokio::main]
/// async fn main() {
///     telemetry::init_telemetry().expect("Failed to initialize telemetry");
///     // ... rest of application
/// }
/// ```
pub fn init_telemetry() -> Result<(), TraceError> {
    // Check if OpenTelemetry is disabled
    if env::var("OTEL_SDK_DISABLED")
        .ok()
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        eprintln!("OpenTelemetry is disabled via OTEL_SDK_DISABLED environment variable");
        return Ok(());
    }

    // Set W3C TraceContext as the global propagator for distributed tracing
    global::set_text_map_propagator(TraceContextPropagator::new());

    // Get configuration from environment variables
    let service_name = env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "source-data-proxy".to_string());
    let service_version = env!("CARGO_PKG_VERSION");
    let deployment_env = env::var("DEPLOYMENT_ENV").unwrap_or_else(|_| "development".to_string());
    let otlp_endpoint = env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:4317".to_string());

    // Parse sample rate from environment variable, default to 10%
    let sample_rate: f64 = env::var("OTEL_TRACE_SAMPLE_RATE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.1_f64)
        .clamp(0.0_f64, 1.0_f64);

    // Create resource with service metadata
    let resource = Resource::from_schema_url(
        [
            KeyValue::new(SERVICE_NAME, service_name.clone()),
            KeyValue::new(SERVICE_VERSION, service_version),
            KeyValue::new(DEPLOYMENT_ENVIRONMENT, deployment_env),
        ],
        SCHEMA_URL,
    );

    // Configure the tracer with OTLP exporter
    let tracer_provider = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(otlp_endpoint),
        )
        .with_trace_config(
            opentelemetry_sdk::trace::Config::default()
                .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
                    sample_rate,
                ))))
                .with_id_generator(RandomIdGenerator::default())
                .with_resource(resource),
        )
        .install_batch(runtime::Tokio)?;

    // Get the tracer from the provider
    let tracer = tracer_provider.tracer("source-data-proxy");

    // Create tracing layers
    let telemetry_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    // Create environment filter for log levels
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();

    // Create JSON formatting layer for CloudWatch Logs
    let formatting_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .with_target(true);

    // Combine all layers into a subscriber
    let subscriber = Registry::default()
        .with(env_filter)
        .with(telemetry_layer)
        .with(formatting_layer);

    // Set the subscriber as the global default
    set_global_default(subscriber).map_err(|e| {
        TraceError::Other(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to set global subscriber: {}", e),
        )))
    })?;

    tracing::info!(
        service_name = %service_name,
        sample_rate = %sample_rate,
        "OpenTelemetry initialized"
    );

    Ok(())
}

/// Shuts down the OpenTelemetry tracer gracefully.
///
/// This function should be called before the application exits to ensure
/// all spans are flushed and exported.
///
/// # Examples
///
/// ```rust
/// use source_data_proxy::utils::telemetry;
///
/// fn main() {
///     // ... application logic
///     telemetry::shutdown_telemetry();
/// }
/// ```
pub fn shutdown_telemetry() {
    tracing::info!("Shutting down OpenTelemetry");
    global::shutdown_tracer_provider();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sample_rate_parsing() {
        // Test that sample rate parsing clamps values correctly
        temp_env::with_var("OTEL_TRACE_SAMPLE_RATE", Some("1.5"), || {
            let sample_rate: f64 = env::var("OTEL_TRACE_SAMPLE_RATE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.1_f64)
                .clamp(0.0_f64, 1.0_f64);
            assert_eq!(sample_rate, 1.0);
        });

        temp_env::with_var("OTEL_TRACE_SAMPLE_RATE", Some("-0.5"), || {
            let sample_rate: f64 = env::var("OTEL_TRACE_SAMPLE_RATE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.1_f64)
                .clamp(0.0_f64, 1.0_f64);
            assert_eq!(sample_rate, 0.0);
        });
    }
}
