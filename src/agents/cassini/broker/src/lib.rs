use cassini_types::ClientMessage;
use opentelemetry::Context;
use ractor::{ActorRef, RpcReplyPort};

pub mod broker;
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
pub const DISPATCH_NAME: &str = "DISPATCH";

pub fn init_logging() {
    use opentelemetry::{global, KeyValue};
    use opentelemetry_otlp::Protocol;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::Resource;
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    // ========
    // --- Initialize tracing + OpenTelemetry ---

    let jaeger_otlp_endpoint = std::env::var("JAEGER_OTLP_ENDPOINT")
        .unwrap_or("http://localhost:4318/v1/traces".to_string());

    // Initialize OTLP exporter using HTTP binary protocol
    let otlp_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(jaeger_otlp_endpoint)
        .build()
        .expect("Expected to build OTLP exporter for tracing.");

    // Create a tracer provider with the exporter and name the service for jaeger
    let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(otlp_exporter)
        .with_resource(
            Resource::builder_empty()
                .with_attributes([KeyValue::new("service.name", "cassini-server")])
                .build(),
        )
        .build();

    // Set it as the global provider
    global::set_tracer_provider(tracer_provider);

    let tracer = global::tracer("cassini_trace");

    // TODO: We copied this from a ractor example, but is this format the one we want to use?
    // let fmt = tracing_subscriber::fmt::Layer::default()
    //     .with_ansi(stderr().is_terminal())
    //     .with_writer(std::io::stderr)
    //     .event_format(Glog::default().with_timer(tracing_glog::LocalTime::default()))
    //     .fmt_fields(GlogFields::default().compact());

    // Set up a filter for logs,
    // by default we'll set the logging to info level if RUST_LOG isn't set in the environment.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .init();
}
/// Internal messagetypes for the Broker.
/// Activities that flow from an actor will also be traced leveraging Contexts,
/// These are optional because they aren't initialzied until the listener begins to handle the message
/// Because of this, they will be often left out of the listener's handler at first
#[derive(Debug, Clone)]
pub enum BrokerMessage {
    /// Registration request from the client.
    /// When a client connects over TCP, it cannot send messages until it receives a registrationID and a session has been created for it
    /// In the event of a disconnect, a client should be able to either resume their session by providing that registration ID, or
    /// have a new one assigned to it by sending an empty registration request
    RegistrationRequest {
        //Id for a new, potentially unauthenticated/unauthorized client client
        registration_id: Option<String>,
        client_id: String,
        trace_ctx: Option<Context>,
    },
    /// Helper variant to start a session and re/initialize it with a new client connection + tracing context
    InitSession {
        client_id: String,
        trace_ctx: Option<Context>,
    },
    /// TODO: Do we care to keep this around?
    /// A heartbeat tick messgae sent by sessions to track uptime
    HeartbeatTick,
    /// Registration response to the client after attempting registration
    /// Ok result contains new registration id,
    /// Err shoudl contain an error message
    RegistrationResponse {
        client_id: String,

        result: Result<String, String>,
        trace_ctx: Option<Context>,
    },
    /// Publish request from the client.
    PublishRequest {
        registration_id: String,
        topic: String,
        payload: Vec<u8>,
        trace_ctx: Option<Context>,
    },
    /// Publish response to the client.
    PublishResponse {
        topic: String,
        payload: Vec<u8>,
        result: Result<(), String>,
        trace_ctx: Option<Context>,
    },
    /// Message sent to the client to let them know they successfully published a message
    PublishRequestAck {
        topic: String,
        trace_ctx: Option<Context>,
    },
    PublishResponseAck,
    /// Subscribe request from the client.
    SubscribeRequest {
        registration_id: String,
        topic: String,
        trace_ctx: Option<Context>,
    },
    /// Sent to the subscriber manager to create a new subscriber actor to handle pushing messages to the client.
    /// If successful, the associated topic actor is notified, adding the id of the new actor to it's subscriber list
    Subscribe {
        topic: String,
        registration_id: String,
        trace_ctx: Option<Context>,
    },
    /// instructs the topic manager to create a new topic actor,
    /// optionally at the behest of a session client during the processing of a SubscribeRequest
    /// which would also prompt the creation of a subscriber agent for that topic.
    AddTopic {
        registration_id: Option<String>,
        topic: String,
        trace_ctx: Option<Context>,
    },
    /// Sent to session actors to forward messages to their clients.
    /// Messages that fail to be delivered for some reason are kept in their queues.
    PushMessage {
        // reply: RpcReplyPort<Result<(), String>>,
        payload: Vec<u8>,
        topic: String,
        trace_ctx: Option<Context>,
    },
    /// Sent back to subscription actors if sessions fail to forward messages to the client for requeueing
    PushMessageFailed {
        payload: Vec<u8>,
    },
    /// Subscribe acknowledgment to the client.
    SubscribeAcknowledgment {
        registration_id: String,
        topic: String,
        result: Result<(), String>, // Ok for success, Err with error message
        trace_ctx: Option<Context>,
    },
    /// Unsubscribe request from the client.
    UnsubscribeRequest {
        registration_id: String,
        topic: String,
        trace_ctx: Option<Context>,
    },
    /// Unsubscribe acknowledgment to the client.
    UnsubscribeAcknowledgment {
        registration_id: String,
        topic: String,
        result: Result<(), String>, // Ok for success, Err with error message
    },
    /// Disconnect request from the client.
    DisconnectRequest {
        client_id: String,
        registration_id: Option<String>,
        trace_ctx: Option<Context>,
    },
    /// Error message to the client.
    ErrorMessage {
        client_id: String,
        error: String,
    },
    /// Ping message to the client to check connectivity.
    PingMessage {
        registration_id: String,
        client_id: String,
    },
    /// Pong message received from the client in response to a ping.
    PongMessage {
        registration_id: String,
    },
    TimeoutMessage {
        client_id: String,
        ///name of the session agent that died
        registration_id: String,
        error: Option<String>,
        // trace_ctx: Option<Context>,
    },
}

impl BrokerMessage {
    pub fn from_client_message(msg: ClientMessage, client_id: String) -> Self {
        match msg {
            ClientMessage::RegistrationRequest { registration_id } => {
                BrokerMessage::RegistrationRequest {
                    registration_id,
                    client_id,
                    trace_ctx: None,
                }
            }
            ClientMessage::PublishRequest {
                topic,
                payload,
                registration_id,
            } => BrokerMessage::PublishRequest {
                registration_id,
                topic,
                payload,
                trace_ctx: None,
            },
            ClientMessage::SubscribeRequest {
                topic,
                registration_id,
            } => BrokerMessage::SubscribeRequest {
                registration_id,
                topic,
                trace_ctx: None,
            },
            ClientMessage::UnsubscribeRequest {
                registration_id,
                topic,
            } => BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
                trace_ctx: None,
            },

            ClientMessage::DisconnectRequest(registration_id) => BrokerMessage::DisconnectRequest {
                client_id,
                registration_id,
                trace_ctx: None,
            },
            // Handle unexpected messages
            _ => {
                todo!("Handle a conversion case between broker messages and client messages")
            }
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
