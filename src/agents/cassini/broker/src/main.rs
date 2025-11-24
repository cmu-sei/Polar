#![allow(clippy::incompatible_msrv)]
use cassini_broker::{
    broker::{Broker, BrokerArgs},
    BROKER_NAME,
};
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::Protocol;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use ractor::Actor;
use std::env;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// ============================== Main ============================== //

#[tokio::main]
async fn main() {
    cassini_broker::init_logging();
    // // --- Initialize tracing + OpenTelemetry ---

    // let jaeger_otlp_endpoint = std::env::var("JAEGER_OTLP_ENDPOINT")
    //     .unwrap_or("http://localhost:4318/v1/traces".to_string());

    // // Initialize OTLP exporter using HTTP binary protocol
    // let otlp_exporter = opentelemetry_otlp::SpanExporter::builder()
    //     .with_http()
    //     .with_protocol(Protocol::HttpBinary)
    //     .with_endpoint(jaeger_otlp_endpoint)
    //     .build()
    //     .expect("Expected to build OTLP exporter for tracing.");

    // // Create a tracer provider with the exporter and name the service for jaeger
    // let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
    //     .with_batch_exporter(otlp_exporter)
    //     .with_resource(
    //         Resource::builder_empty()
    //             .with_attributes([KeyValue::new("service.name", "cassini-server")])
    //             .build(),
    //     )
    //     .build();

    // // Set it as the global provider
    // global::set_tracer_provider(tracer_provider);

    // let tracer = global::tracer("cassini_trace");

    // // TODO: We copied this from a ractor example, but is this format the one we want to use?
    // // let fmt = tracing_subscriber::fmt::Layer::default()
    // //     .with_ansi(stderr().is_terminal())
    // //     .with_writer(std::io::stderr)
    // //     .event_format(Glog::default().with_timer(tracing_glog::LocalTime::default()))
    // //     .fmt_fields(GlogFields::default().compact());

    // // Set up a filter for logs,
    // // by default we'll set the logging to info level if RUST_LOG isn't set in the environment.
    // let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // tracing_subscriber::registry()
    //     .with(filter)
    //     .with(tracing_subscriber::fmt::layer())
    //     .with(tracing_opentelemetry::layer().with_tracer(tracer))
    //     .init();

    let server_cert_file = env::var("TLS_SERVER_CERT_CHAIN")
        .expect("Expected a value for the TLS_SERVER_CERT_CHAIN environment variable.");
    let private_key_file = env::var("TLS_SERVER_KEY")
        .expect("Expected a value for the TLS_SERVER_KEY environment variable.");
    let ca_cert_file = env::var("TLS_CA_CERT")
        .expect("Expected a value for the TLS_CA_CERT environment variable.");
    let bind_addr = env::var("CASSINI_BIND_ADDR").unwrap_or(String::from("0.0.0.0:8080"));

    let args = BrokerArgs {
        bind_addr,
        session_timeout: None,
        server_cert_file,
        private_key_file,
        ca_cert_file,
    };

    // Start Supervisor
    let (_broker, handle) = Actor::spawn(Some(BROKER_NAME.to_string()), Broker, args)
        .await
        .expect("Failed to start Broker");

    handle.await.expect("Something went wrong");
}
