use cassini::client::*;
use cassini::TCPClientConfig;
use jira_common::get_file_as_byte_vec;
use ractor::async_trait;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::OutputPort;
use ractor::SupervisionEvent;
use reqwest::Certificate;
use reqwest::Client;
use reqwest::ClientBuilder;
use tracing::error;
use tracing::{debug, info, warn};

use crate::projects::JiraProjectObserver;
use crate::groups::JiraGroupObserver;
use crate::users::JiraUserObserver;
use crate::issues::JiraIssueObserver;
use crate::JiraObserverArgs;
use crate::BROKER_CLIENT_NAME;
use crate::JIRA_PROJECT_OBSERVER;
use crate::JIRA_GROUP_OBSERVER;
use crate::JIRA_USER_OBSERVER;
use crate::JIRA_ISSUE_OBSERVER;
pub struct ObserverSupervisor;

pub struct ObserverSupervisorState {
    pub jira_url: String,
    pub jira_token: Option<String>,
    /// base interval that observers will use to query
    pub base_interval: u64,
    pub max_backoff_secs: u64,
    pub proxy_ca_cert_file: Option<String>,
}

pub struct ObserverSupervisorArgs {
    pub client_config: TCPClientConfig,
    pub jira_url: String,
    pub jira_token: Option<String>,
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

pub enum SupervisorMessage {
    /// Notification message telling the supervisor the client's been registered with the broker.
    /// This triggers the observer to finish startup and cancels the timeout
    ClientRegistered(String),
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

        // define an output port for the actor to subscribe to
        let output_port = std::sync::Arc::new(OutputPort::default());

        // subscribe self to this port
        output_port.subscribe(myself.clone(), |message| {
            Some(SupervisorMessage::ClientRegistered(message))
        });

        match Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config: args.client_config,
                registration_id: None,
                output_port,
            },
            myself.clone().into(),
        )
        .await
        {
            // set up and return state
            Ok(_) => Ok(ObserverSupervisorState {
                jira_url: args.jira_url.clone(),
                jira_token: args.jira_token.clone(),
                base_interval: args.base_interval,
                max_backoff_secs: args.max_backoff_secs,
                proxy_ca_cert_file: args.proxy_ca_cert_file,
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
            SupervisorMessage::ClientRegistered(registration_id) => {
                // set up args for observer actors
                let args = JiraObserverArgs {
                    jira_url: state.jira_url.clone(),
                    token: state.jira_token.clone(),
                    registration_id: registration_id.clone(),
                    web_client: ObserverSupervisor::get_client(state.proxy_ca_cert_file.clone()),
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
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                error!(
                    "OBSERVER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
                // at time of writing, if any observers terminate
                // it's a likely unrecoverable state -
                // it could be caused by an invalid jira token being provided or any malformed query.
                // this would require intervention by admins.
                myself.stop(reason)
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                error!(
                    "OBSERVER_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
                myself.stop(Some(e.to_string()))
            }
            SupervisionEvent::ProcessGroupChanged(..) => {
                todo!("Investigate how this would/could happen and how to respond.")
            }
        }

        Ok(())
    }
}
