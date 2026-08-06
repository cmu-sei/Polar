use crate::BROKER_CLIENT_NAME;
use crate::JIRA_GROUP_OBSERVER;
use crate::JIRA_ISSUE_OBSERVER;
use crate::JIRA_PROJECT_OBSERVER;
use crate::JIRA_USER_OBSERVER;
use crate::JiraAuth;
use crate::JiraDeployment;
use crate::JiraObserverArgs;
use crate::groups::JiraGroupObserver;
use crate::issues::JiraIssueObserver;
use crate::projects::JiraProjectObserver;
use crate::users::JiraUserObserver;
use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs};
use cassini_types::ClientEvent;
use jira_common::get_file_as_byte_vec;
use polar::SupervisorMessage;
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::OutputPort;
use ractor::SupervisionEvent;
use ractor::async_trait;
use ractor::concurrency::Duration;
use reqwest::Certificate;
use reqwest::Client;
use reqwest::ClientBuilder;
use std::sync::Arc;
use tracing::error;
use tracing::{debug, info, warn};

const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

pub struct ObserverSupervisor;

pub struct ObserverSupervisorState {
    pub jira_url: String,
    pub auth: JiraAuth,
    pub deployment: JiraDeployment,
    pub base_interval: u64,
    pub max_backoff_secs: u64,
    pub proxy_ca_cert_file: Option<String>,
    healthcheck: ActorRef<HealthCheckMessage>,
    draining: bool,
}

pub struct ObserverSupervisorArgs {
    pub jira_url: String,
    pub auth: JiraAuth,
    pub deployment: JiraDeployment,
    pub proxy_ca_cert_file: Option<String>,
    pub base_interval: u64,
    pub max_backoff_secs: u64,
}

impl ObserverSupervisor {
    /// Build reqwest client, optionally with a proxy CA certificate
    fn get_client(proxy_ca_cert_path: Option<String>) -> Client {
        match proxy_ca_cert_path {
            Some(path) => {
                let cert_data = get_file_as_byte_vec(&path)
                    .expect("Expected to find a proxy CA certificate at {path}");
                let root_cert = Certificate::from_pem(&cert_data)
                    .expect("Expected {path} to be in PEM format.");

                info!("Found PROXY_CA_CERT at: {path}, Configuring web client...");

                ClientBuilder::new()
                    .add_root_certificate(root_cert)
                    .use_rustls_tls()
                    .build()
                    .expect("Expected to build web client with proxy CA certificate")
            }
            None => ClientBuilder::new()
                .build()
                .expect("Expected to build web client."),
        }
    }
}

#[async_trait]
impl Actor for ObserverSupervisor {
    type Msg = SupervisorMessage;
    type State = ObserverSupervisorState;
    type Arguments = ObserverSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ObserverSupervisorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let prepare_shutdown_port = Arc::new(OutputPort::<()>::default());
        prepare_shutdown_port.subscribe(myself.clone(), |()| {
            Some(SupervisorMessage::PrepareShutdown)
        });

        // Observers only depend on Cassini -- no graph dependency.
        let dep_cert_endpoints = vec![
            DepCertEndpoint::parse("cassini-ip-svc.polar.svc.cluster.local:8080:300")
                .map_err(|e| ActorProcessingErr::from(e))?,
        ];

        let (healthcheck, _) = HealthCheckActor::spawn_linked(
            Some(HEALTHCHECK_ACTOR_NAME.to_string()),
            HealthCheckActor,
            HealthCheckArgs {
                expects_graph: false,
                rejuvenation_threshold_secs: 300,
                dep_cert_endpoints,
                prepare_shutdown_port,
            },
            myself.get_cell(),
        )
        .await
        .map_err(|e| ActorProcessingErr::from(e))?;

        let events_output = std::sync::Arc::new(OutputPort::default());
        events_output.subscribe(myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        });

        let config = TCPClientConfig::new()?;

        match Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config,
                registration_id: None,
                events_output: Some(events_output),
                event_handler: None,
            },
            myself.clone().into(),
        )
        .await
        {
            Ok(_) => Ok(ObserverSupervisorState {
                jira_url: args.jira_url.clone(),
                auth: args.auth.clone(),
                deployment: args.deployment,
                base_interval: args.base_interval,
                max_backoff_secs: args.max_backoff_secs,
                proxy_ca_cert_file: args.proxy_ca_cert_file,
                healthcheck,
                draining: false,
            }),
            Err(e) => {
                error!("{e}");
                Err(ActorProcessingErr::from(e))
            }
        }
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::Heartbeat => {}
            SupervisorMessage::PrepareShutdown => {
                info!("OBSERVER_SUPERVISOR: PrepareShutdown received");
                // Observers only publish -- nothing to unsubscribe, no
                // discrete queue to drain. Ack immediately.
                if let Err(e) = state.healthcheck.cast(HealthCheckMessage::ShutdownAck) {
                    error!("OBSERVER_SUPERVISOR: failed to send ShutdownAck: {e}");
                }
            }
            SupervisorMessage::GraphSignal(_) => {}
            SupervisorMessage::ForceExit => {
                warn!("OBSERVER_SUPERVISOR: drain window elapsed, exiting now");
                std::process::exit(1);
            }
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { registration_id } => {
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);

                    let args = JiraObserverArgs {
                        jira_url: state.jira_url.clone(),
                        auth: state.auth.clone(),
                        deployment: state.deployment,
                        registration_id: registration_id.clone(),
                        web_client: ObserverSupervisor::get_client(
                            state.proxy_ca_cert_file.clone(),
                        ),
                        base_interval: state.base_interval,
                        max_backoff: state.max_backoff_secs,
                    };
                    if let Err(e) = Actor::spawn_linked(
                        Some(JIRA_PROJECT_OBSERVER.to_string()),
                        JiraProjectObserver,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        warn!("failed to start project observer {e}")
                    }

                    if let Err(e) = Actor::spawn_linked(
                        Some(JIRA_GROUP_OBSERVER.to_string()),
                        JiraGroupObserver,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        warn!("failed to start group observer {e}")
                    }
                    if let Err(e) = Actor::spawn_linked(
                        Some(JIRA_USER_OBSERVER.to_string()),
                        JiraUserObserver,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        warn!("failed to start user observer {e}")
                    }
                    if let Err(e) = Actor::spawn_linked(
                        Some(JIRA_ISSUE_OBSERVER.to_string()),
                        JiraIssueObserver,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        warn!("failed to start issue observer {e}")
                    }
                }
                ClientEvent::MessagePublished { .. } => {
                    if state.draining {
                        warn!("OBSERVER_SUPERVISOR: draining -- ignoring unexpected message");
                    } else {
                        warn!("OBSERVER_SUPERVISOR: unexpected MessagePublished event received, ignoring");
                    }
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniDisconnected);
                }
                _ => (),
            },
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                let actor_name = actor_cell.get_name().unwrap_or_default();
                error!(
                    "OBSERVER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_name,
                    actor_cell.get_id()
                );

                if actor_name == HEALTHCHECK_ACTOR_NAME {
                    if !state.draining {
                        state.draining = true;
                        info!(
                            "OBSERVER_SUPERVISOR: healthcheck actor terminated cleanly, \
                             entering drain window before exiting for rejuvenation"
                        );
                        let _ = myself
                            .send_after(Duration::from_secs(DRAIN_WINDOW_SECS), || {
                                SupervisorMessage::ForceExit
                            })
                            .await;
                    }
                    return Ok(());
                }

                // At time of writing, any other observer terminating is a
                // likely unrecoverable state (invalid Jira token, malformed
                // query) requiring admin intervention. Preserve that
                // existing behavior, but route through the same bounded
                // drain window rather than stopping outright with no exit
                // trigger.
                if !state.draining {
                    state.draining = true;
                    warn!(
                        "OBSERVER_SUPERVISOR: entering drain window before exit; \
                         likely unrecoverable state (see terminated actor above)"
                    );
                    let _ = myself
                        .send_after(Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                error!(
                    "OBSERVER_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
                if !state.draining {
                    state.draining = true;
                    warn!(
                        "OBSERVER_SUPERVISOR: entering drain window before exit; \
                         any messages that arrive will be ignored"
                    );
                    let _ = myself
                        .send_after(Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
            }
            SupervisionEvent::ProcessGroupChanged(..) => {
                todo!("Investigate how this would/could happen and how to respond.")
            }
        }

        Ok(())
    }
}
