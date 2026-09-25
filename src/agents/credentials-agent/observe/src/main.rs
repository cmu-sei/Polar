use axum::Router;
use axum::routing::post;
use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::ClientEvent;
use credentials_agent_common::identity::IdentityNormalizationReply;
use credentials_agent_common::issuance::IssuanceReply;
use credentials_agent_common::{
    CREDENTIALS_IDENTITY_NORMALIZATION_REPLIES_TOPIC, CREDENTIALS_ISSUANCE_REPLIES_TOPIC,
};
use credentials_agent_observer::handler::{ObserverState, identity, issue};
use polar::request_reply::{Envelope, ReplyRouter};
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, async_trait, registry::where_is};
use std::sync::Arc;
use tracing::{error, info, warn};

const SERVICE_NAME: &str = "credentials-agent-observer";
const BROKER_CLIENT_NAME: &str = "CREDENTIALS_AGENT_OBSERVER_CLIENT";
const HEALTHCHECK_ACTOR_NAME: &str = "polar.credentials-agent-observer.healthcheck";

/// The one, real job this actor exists for: deliver incoming Cassini
/// replies on the two reply topics handler.rs's own issue()/identity()
/// functions are actually waiting on, into their real ReplyRouters --
/// mirroring polar-crypto's own, established ClientEvent-subscription
/// pattern, minimized to just what this observer's own two, real
/// dispatch paths need.
struct ReplyRouterActor {
    issuance_reply_router: Arc<ReplyRouter<IssuanceReply>>,
    identity_reply_router: Arc<ReplyRouter<IdentityNormalizationReply>>,
}

enum ReplyRouterMessage {
    ClientEvent(ClientEvent),
}

#[async_trait]
impl Actor for ReplyRouterActor {
    type Msg = ReplyRouterMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let ReplyRouterMessage::ClientEvent(event) = message;
        match event {
            ClientEvent::MessagePublished { topic, payload, .. }
                if topic == CREDENTIALS_ISSUANCE_REPLIES_TOPIC =>
            {
                match rkyv::from_bytes::<Envelope<IssuanceReply>, rkyv::rancor::Error>(&payload) {
                    Ok(envelope) => self
                        .issuance_reply_router
                        .deliver(&envelope.correlation_id, envelope.payload),
                    Err(e) => warn!("failed to deserialize issuance reply: {e}"),
                }
            }
            ClientEvent::MessagePublished { topic, payload, .. }
                if topic == CREDENTIALS_IDENTITY_NORMALIZATION_REPLIES_TOPIC =>
            {
                match rkyv::from_bytes::<Envelope<IdentityNormalizationReply>, rkyv::rancor::Error>(
                    &payload,
                ) {
                    Ok(envelope) => self
                        .identity_reply_router
                        .deliver(&envelope.correlation_id, envelope.payload),
                    Err(e) => warn!("failed to deserialize identity-normalization reply: {e}"),
                }
            }
            ClientEvent::Registered { .. } => {
                info!("ClientEvent::Registered received — subscribing to reply topics");
                if let Some(healthcheck) = where_is(HEALTHCHECK_ACTOR_NAME.to_string()) {
                    let healthcheck: ActorRef<HealthCheckMessage> = healthcheck.into();
                    let _ = healthcheck.cast(HealthCheckMessage::CassiniConnected);
                } else {
                    error!("healthcheck actor not found while attempting to notify it of connection");
                }
                if let Some(client) = ractor::registry::where_is(BROKER_CLIENT_NAME.to_string()) {
                    for topic in [
                        CREDENTIALS_ISSUANCE_REPLIES_TOPIC,
                        CREDENTIALS_IDENTITY_NORMALIZATION_REPLIES_TOPIC,
                    ] {
                        if let Err(e) = client.send_message(TcpClientMessage::Subscribe {
                            topic: topic.to_string(),
                            trace_ctx: None,
                        }) {
                            error!("failed to subscribe to {topic}: {e}");
                        }
                    }
                } else {
                    error!("broker client not found while attempting to subscribe");
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    polar::init_logging(SERVICE_NAME.to_string());

    let dep_cert_endpoints = vec![
        DepCertEndpoint::parse("cassini-ip-svc.polar.svc.cluster.local:8080:300")
            .expect("cassini dep cert endpoint must parse"),
    ];

    let (_healthcheck, _) = Actor::spawn(
        Some(HEALTHCHECK_ACTOR_NAME.to_string()),
        HealthCheckActor,
        HealthCheckArgs {
            expects_graph: false,
            rejuvenation_threshold_secs: 300,
            dep_cert_endpoints,
        },
    )
    .await
    .expect("Expected healthcheck actor to start");

    let issuance_reply_router = Arc::new(ReplyRouter::new());
    let identity_reply_router = Arc::new(ReplyRouter::new());

    let (reply_router_actor, _) = Actor::spawn(
        Some(format!("{SERVICE_NAME}.reply-router")),
        ReplyRouterActor {
            issuance_reply_router: issuance_reply_router.clone(),
            identity_reply_router: identity_reply_router.clone(),
        },
        (),
    )
    .await
    .expect("Expected reply-router actor to start");

    let events_output = Arc::new(OutputPort::default());
    events_output.subscribe(reply_router_actor.clone(), |event| {
        Some(ReplyRouterMessage::ClientEvent(event))
    });

    let client_config = TCPClientConfig::new().expect("Cassini client config must be readable");

    let (broker_client, _) = Actor::spawn(
        Some(BROKER_CLIENT_NAME.to_string()),
        TcpClientActor,
        TcpClientArgs {
            config: client_config,
            registration_id: None,
            delivery: cassini_client::DeliveryMode::Broadcast(events_output),
        },
    )
    .await
    .expect("Expected Cassini client to start");

    let state = Arc::new(ObserverState {
        broker_client,
        issuance_reply_router,
        identity_reply_router,
    });

    let app = Router::new()
        .route("/issue", post(issue))
        .route("/identity", post(identity))
        .with_state(state);

    let port = std::env::var("CREDENTIALS_AGENT_OBSERVER_PORT").unwrap_or_else(|_| "8080".to_string());
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("Expected to bind observer's own HTTP listener");
    tracing::info!("{SERVICE_NAME} listening on 0.0.0.0:{port}");

    axum::serve(listener, app)
        .await
        .expect("Expected observer's own HTTP server to run cleanly");
}
