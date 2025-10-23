#![allow(clippy::incompatible_msrv)]
use cassini_broker::{
    broker::{Broker, BrokerArgs},
    BROKER_NAME,
};
use ractor::Actor;
use std::{
    env,
    io::{stderr, IsTerminal},
};
use tracing_glog::Glog;
use tracing_glog::GlogFields;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::Registry;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// ============================== Main ============================== //

#[tokio::main]
async fn main() {
    // --- Initialize tracing + OpenTelemetry ---

    let tracer = opentelemetry::global::tracer("cassini_trace");

    // TODO: We copied this from a ractor example, but is this format the one we want to use?
    // let fmt = tracing_subscriber::fmt::Layer::default()
    //     .with_ansi(stderr().is_terminal())
    //     .with_writer(std::io::stderr)
    //     .event_format(Glog::default().with_timer(tracing_glog::LocalTime::default()))
    //     .fmt_fields(GlogFields::default().compact());

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .init();

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
