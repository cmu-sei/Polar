/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

//! Distributed tracing initialisation for Cassini components.
//! Call `init_tracing(service_name)` early in `main()` to set up
//! console logging and (optionally) OpenTelemetry export to Jaeger.

use cassini_types::WireTraceCtx;
use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::{
    Resource,
    trace::{self, RandomIdGenerator},
};
use std::io::{IsTerminal, stderr};
use std::sync::Mutex;
use tracing::Span;
use tracing_glog::Glog;
use tracing_glog::GlogFields;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};

use opentelemetry::Context;
use std::panic::{AssertUnwindSafe, catch_unwind};

static TRACER_PROVIDER: Mutex<Option<opentelemetry_sdk::trace::SdkTracerProvider>> =
    Mutex::new(None);

/// What logging and tracing are set up with. A service that declares its configuration
/// (the broker) hands these over; `init_tracing` reads them from the process environment, as it
/// always did.
#[derive(Debug, Clone, Default)]
pub struct TracingSettings {
    /// The console filter, in tracing's syntax. `warn` when unset or when it does not parse.
    pub log_filter: Option<String>,
    /// Which spans and events are exported, in tracing's syntax. `debug` when unset or when it
    /// does not parse.
    pub trace_filter: Option<String>,
    /// Where spans are exported over OTLP HTTP. The local collector's port when unset.
    pub otlp_endpoint: Option<String>,
}

impl TracingSettings {
    /// `CASSINI_LOG`, `RUST_LOG` and `JAEGER_OTLP_ENDPOINT` from the process environment.
    pub fn from_env() -> Self {
        Self {
            log_filter: std::env::var("CASSINI_LOG").ok(),
            trace_filter: std::env::var("RUST_LOG").ok(),
            otlp_endpoint: std::env::var("JAEGER_OTLP_ENDPOINT").ok(),
        }
    }

    fn trace_filter(&self) -> EnvFilter {
        self.trace_filter
            .as_deref()
            .and_then(|filter| EnvFilter::try_new(filter).ok())
            .unwrap_or_else(|| EnvFilter::new("debug"))
    }

    fn console_filter(&self) -> EnvFilter {
        self.log_filter
            .as_deref()
            .and_then(|filter| EnvFilter::try_new(filter).ok())
            .unwrap_or_else(|| EnvFilter::new("warn"))
    }

    fn endpoint(&self) -> String {
        self.otlp_endpoint
            .clone()
            .unwrap_or_else(|| "http://localhost:4318/v1/traces".to_string())
    }
}

/// Initialises logging and tracing from the process environment: see
/// [`init_tracing_with`].
pub fn init_tracing(service_name: &str) {
    init_tracing_with(service_name, &TracingSettings::from_env());
}

/// Initialises logging and tracing.
///
/// - Always logs human‑readable lines to stderr (with optional ANSI colours).
/// - Exports spans via OTLP HTTP to the settings' endpoint.
/// - The `service.name` attribute in traces is set to the provided `service_name`.
pub fn init_tracing_with(service_name: &str, settings: &TracingSettings) {
    let console_filter = settings.console_filter();

    let jaeger_filter = settings.trace_filter();

    let endpoint = settings.endpoint();

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
                    .with_attributes([KeyValue::new("service.version", env!("CARGO_PKG_VERSION"))])
                    .build(),
            )
            .build();

        let _ = TRACER_PROVIDER
            .lock()
            .unwrap()
            .replace(tracer_provider.clone());
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
    let console_filter = settings.console_filter();

    let fmt = tracing_subscriber::fmt::layer()
        .pretty()
        .with_ansi(stderr().is_terminal())
        .with_writer(std::io::stderr)
        .event_format(Glog::default().with_timer(tracing_glog::LocalTime::default()))
        .fmt_fields(GlogFields::default().compact())
        .with_filter(console_filter);

    if tracing_subscriber::registry().with(fmt).try_init().is_err() {
        eprintln!("Logging registry already initialized");
    }
}

/// Shuts down the tracing provider, flushing any pending spans.
/// Should be called before the process exits.
pub fn shutdown_tracing() {
    if let Some(provider) = TRACER_PROVIDER.lock().unwrap().take()
        && let Err(e) = provider.shutdown()
    {
        eprintln!("Error shutting down tracer provider: {:?}", e);
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
        tracing::error!(
            "Failed to set parent span (likely race condition), continuing without parent"
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_set_logs_warnings_and_exports_to_the_local_collector() {
        let settings = TracingSettings::default();
        assert_eq!(settings.console_filter().to_string(), "warn");
        assert_eq!(settings.trace_filter().to_string(), "debug");
        assert_eq!(settings.endpoint(), "http://localhost:4318/v1/traces");
    }

    #[test]
    fn what_is_set_is_used() {
        let settings = TracingSettings {
            log_filter: Some("info".to_string()),
            trace_filter: Some("polar=info".to_string()),
            otlp_endpoint: Some("http://jaeger:4318/v1/traces".to_string()),
        };
        assert_eq!(settings.console_filter().to_string(), "info");
        assert_eq!(settings.trace_filter().to_string(), "polar=info");
        assert_eq!(settings.endpoint(), "http://jaeger:4318/v1/traces");
    }

    #[test]
    fn a_filter_that_does_not_parse_falls_back_to_warn_instead_of_silencing_the_log() {
        let settings = TracingSettings {
            log_filter: Some("not=a=filter[".to_string()),
            trace_filter: Some("not=a=filter[".to_string()),
            otlp_endpoint: None,
        };
        assert_eq!(settings.console_filter().to_string(), "warn");
        assert_eq!(settings.trace_filter().to_string(), "debug");
    }
}
