//! Distributed tracing initialisation for Cassini components.
//! Call `init_tracing(service_name)` early in `main()` to set up
//! console logging and (optionally) OpenTelemetry export to Jaeger.

use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::{trace::{self, RandomIdGenerator}, Resource};
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};
use tracing_glog::Glog;
use tracing_glog::GlogFields;
use std::io::{stderr, IsTerminal};
use std::sync::Mutex;
use cassini_types::WireTraceCtx;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use opentelemetry::Context;
use std::panic::{catch_unwind, AssertUnwindSafe};

static TRACER_PROVIDER: Mutex<Option<opentelemetry_sdk::trace::SdkTracerProvider>> = Mutex::new(None);

/// Initialises logging and tracing.
///
/// - Always logs human‑readable lines to stderr (with optional ANSI colours).
/// - If `ENABLE_JAEGER_TRACING=1` and `JAEGER_OTLP_ENDPOINT` is set,
///   also exports spans via OTLP HTTP to the given endpoint.
/// - The `service.name` attribute in traces is set to the provided `service_name`.
pub fn init_tracing(service_name: &str) {
    let console_filter = EnvFilter::try_from_env("CASSINI_LOG")
        .unwrap_or_else(|_| EnvFilter::new("warn"));

    let jaeger_filter = EnvFilter::try_from_env("RUST_LOG")
        .unwrap_or_else(|_| EnvFilter::new("debug"));

    let endpoint = std::env::var("JAEGER_OTLP_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:4318/v1/traces".to_string());

    if let Ok(exporter) = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(endpoint)
        .build()
    {
        let tracer_provider = trace::SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_sampler(trace::Sampler::AlwaysOn)
            .with_id_generator(RandomIdGenerator::default())
            .with_resource(
                Resource::builder()
                    .with_service_name(service_name.to_string())
                    .with_attributes([
                        KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
                    ])
                    .build(),
            )
            .build();

        let _ = TRACER_PROVIDER.lock().unwrap().replace(tracer_provider.clone());
        global::set_tracer_provider(tracer_provider);
        let tracer = global::tracer("cassini-tracing");
        let telemetry_layer = tracing_opentelemetry::layer()
            .with_tracer(tracer)
            .with_filter(jaeger_filter);

        let fmt = tracing_subscriber::fmt::layer()
            .with_ansi(stderr().is_terminal())
            .with_writer(std::io::stderr)
            .event_format(Glog::default().with_timer(tracing_glog::LocalTime::default()))
            .fmt_fields(GlogFields::default().compact())
            .with_filter(console_filter);

        if tracing_subscriber::registry()
            .with(fmt)
            .with(telemetry_layer)
            .try_init()
            .is_err()
        {
            eprintln!("Logging registry already initialized");
        }
        return;
    }

    // Fallback: console only
    let console_filter = EnvFilter::try_from_env("CASSINI_LOG")
        .unwrap_or_else(|_| EnvFilter::new("warn"));

    let fmt = tracing_subscriber::fmt::layer()
        .with_ansi(stderr().is_terminal())
        .with_writer(std::io::stderr)
        .event_format(Glog::default().with_timer(tracing_glog::LocalTime::default()))
        .fmt_fields(GlogFields::default().compact())
        .with_filter(console_filter);

    if tracing_subscriber::registry()
        .with(fmt)
        .try_init()
        .is_err()
    {
        eprintln!("Logging registry already initialized");
    }
}

/// Shuts down the tracing provider, flushing any pending spans.
/// Should be called before the process exits.
pub fn shutdown_tracing () {
     if let Some(provider) = TRACER_PROVIDER.lock().unwrap().take() {
         if let Err(e) = provider.shutdown() {
             eprintln!("Error shutting down tracer provider: {:?}", e);
         }
     }
}

/// Attempts to set a parent span from an OpenTelemetry context, catching any panic
/// that might occur because the parent span has already been closed.
pub fn try_set_parent_otel(span: &Span, otel_ctx: Option<Context>) {
    let Some(ctx) = otel_ctx else { return };
    // Clone the span to take ownership, then wrap the closure in AssertUnwindSafe
    let span = span.clone();
    let result = catch_unwind(AssertUnwindSafe(move || {
        let _ = span.set_parent(ctx);
    }));
    if result.is_err() {
        tracing::error!("Failed to set parent span (likely race condition), continuing without parent");
    }
}

/// Attempts to set a parent span from a wire trace context, catching any panic
/// that might occur because the parent span has already been closed (a known race
/// condition in `tracing-subscriber` under high concurrency).
///
/// If a panic occurs, an error is logged and the function returns without modifying
/// the span.
/// Converts the `WireTraceCtx` to an OpenTelemetry context first.
pub fn try_set_parent_wire(span: &Span, wire_ctx: Option<WireTraceCtx>) {
    let Some(ctx) = wire_ctx else { return };
    let otel_ctx = ctx.to_opentelemetry_context();
    try_set_parent_otel(span, Some(otel_ctx));
}
