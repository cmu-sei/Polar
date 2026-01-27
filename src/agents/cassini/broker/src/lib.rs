use std::{env, fmt};
pub mod broker;
pub mod control;
pub mod listener;
pub mod session;
pub mod subscriber;
pub mod topic;

/// Constant names of core supervisors, should remain constant
/// as they are used to conduct lookups in the ractor registry
pub const BROKER_NAME: &str = "BROKER_SUPERVISOR";
pub const LISTENER_MANAGER_NAME: &str = "LISTENER_MANAGER";
pub const SESSION_MANAGER_NAME: &str = "SESSION_MANAGER";
pub const TOPIC_MANAGER_NAME: &str = "TOPIC_MANAGER";
pub const SUBSCRIBER_MANAGER_NAME: &str = "SUBSCRIBER_MANAGER";
pub const CONTROL_MANAGER_NAME: &str = "CONTROL_MANAGER";
pub const ACTOR_STARTUP_MSG: &str = "Started {myself:?}";
pub const UNEXPECTED_MESSAGE_STR: &str = "Received unexpected message!";
pub const SESSION_NOT_NAMED: &str = "Expected session to have been named.";
pub const SESSION_MISSING_REASON_STR: &str = "SESSION_MISSING";
pub const SESSION_NOT_FOUND_TXT: &str = "Session not found!";
pub const CLIENT_NOT_FOUND_TXT: &str = "Listener not found!";
pub const TOPIC_MGR_NOT_FOUND_TXT: &str = "Topic Manager not found!";
pub const SUBSCRIBER_MGR_NOT_FOUND_TXT: &str = "Subscription Manager not found!";
pub const SESSION_MGR_NOT_FOUND_TXT: &str = "Session Manager not found!";
pub const BROKER_NOT_FOUND_TXT: &str = "Broker not found!";
pub const SUBSCRIBE_REQUEST_FAILED_TXT: &str = "Failed to subscribe to topic";
pub const PUBLISH_REQ_FAILED_TXT: &str = "Failed to publish message to topic";
pub const REGISTRATION_REQ_FAILED_TXT: &str = "Failed to register session!";
pub const LISTENER_MGR_NOT_FOUND_TXT: &str = "Listener Manager not found!";
pub const TIMEOUT_REASON: &str = "SESSION_TIMEDOUT";
pub const DISCONNECTED_REASON: &str = "CLIENT_DISCONNECTED";

#[derive(Debug, Clone)]
pub enum BrokerConfigError {
    EnvVar { var: String, source: env::VarError },
}

impl fmt::Display for BrokerConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use crate::BrokerConfigError::EnvVar;
        match self {
            EnvVar { var, source } => {
                write!(f, "{source}. Pleaes provide a value for {var}")
            }
        }
    }
}

pub fn init_logging() {
    use opentelemetry::{global, KeyValue};
    use opentelemetry_otlp::Protocol;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::Resource;
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let enable_jaeger = std::env::var("ENABLE_JAEGER_TRACING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if enable_jaeger {
        let endpoint = std::env::var("JAEGER_OTLP_ENDPOINT")
            .unwrap_or_else(|_| "http://localhost:4318/v1/traces".to_string());

        if !endpoint.is_empty() {
            if let Ok(exporter) = opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_protocol(Protocol::HttpBinary)
                .with_endpoint(endpoint)
                .build()
            {
                let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
                    .with_batch_exporter(exporter)
                    .with_resource(
                        Resource::builder_empty()
                            .with_attributes([KeyValue::new("service.name", "cassini-server")])
                            .build(),
                    )
                    .build();

                global::set_tracer_provider(tracer_provider);
                let tracer = global::tracer(format!("cassini-tracing"));

                if tracing_subscriber::registry()
                    .with(filter)
                    .with(tracing_subscriber::fmt::layer())
                    .with(tracing_opentelemetry::layer().with_tracer(tracer))
                    .try_init()
                    .is_err()
                {
                    eprintln!("Logging registry already initialized");
                }
            }
        }
    } else {
        if tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .try_init()
            .is_err()
        {
            eprintln!("Logging registry already initialized");
        }
    }
}
///Consider a different naming convention for the subscribers, right now they're named directly after the session they represent and the topic they subscribe to
/// IF we wanted to support topics subscribing to topics e.g overloading the type of subscriber topics can have, we will want to reconsider this approach.
pub fn get_subscriber_name(registration_id: &str, topic: &str) -> String {
    format!("{0}:{1}", registration_id, topic)
}

pub fn parse_host_and_port(endpoint: &str) -> Result<(String, u16), String> {
    // Add scheme if missing so Url::parse works
    let formatted = if endpoint.contains("://") {
        endpoint.to_string()
    } else {
        format!("https://{}", endpoint) // dummy scheme
    };

    let url = url::Url::parse(&formatted).map_err(|e| format!("Invalid endpoint URL: {}", e))?;

    let host = url
        .host_str()
        .ok_or_else(|| "No host found in endpoint".to_string())?
        .to_string();

    let port = url
        .port_or_known_default()
        .ok_or_else(|| "No port found and no default for scheme".to_string())?;

    Ok((host, port))
}
