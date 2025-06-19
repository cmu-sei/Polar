use cassini::client::*;
use cassini::TCPClientConfig;
use common::get_file_as_byte_vec;
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

use crate::groups::GitlabGroupObserver;
use crate::pipelines::GitlabJobObserver;
use crate::pipelines::GitlabPipelineObserver;
use crate::projects::GitlabProjectObserver;
use crate::repositories::GitlabRepositoryObserver;
use crate::runners::GitlabRunnerObserver;
use crate::users::GitlabUserObserver;
use crate::GitlabObserverArgs;
use crate::BROKER_CLIENT_NAME;
use crate::GITLAB_GROUPS_OBSERVER;
use crate::GITLAB_JOBS_OBSERVER;
use crate::GITLAB_PIPELINE_OBSERVER;
use crate::GITLAB_PROJECT_OBSERVER;
use crate::GITLAB_REPOSITORY_OBSERVER;
use crate::GITLAB_RUNNER_OBSERVER;
use crate::GITLAB_USERS_OBSERVER;
pub struct ObserverSupervisor;

pub struct ObserverSupervisorState {
    pub gitlab_endpoint: String,
    pub gitlab_token: Option<String>,
    pub base_interval: u64,
    pub max_backoff_secs: u64,
    pub proxy_ca_cert_file: Option<String>,
}

pub struct ObserverSupervisorArgs {
    pub client_config: TCPClientConfig,
    pub gitlab_endpoint: String,
    pub gitlab_token: Option<String>,
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
        // TODO: Does this make sense? the supervisor is the only one that cares, whether registration is successful after all
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
                gitlab_endpoint: args.gitlab_endpoint.clone(),
                gitlab_token: args.gitlab_token.clone(),
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

    async fn post_start(
        &self,
        _: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
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
                let args = GitlabObserverArgs {
                    gitlab_endpoint: state.gitlab_endpoint.clone(),
                    token: state.gitlab_token.clone(),
                    registration_id: registration_id.clone(),
                    web_client: ObserverSupervisor::get_client(state.proxy_ca_cert_file.clone()),
                    base_interval: state.base_interval,
                    max_backoff: state.max_backoff_secs,
                };

                //TODO: decide which observers to start based off of some configuration
                if let Err(e) = Actor::spawn_linked(
                    Some(GITLAB_USERS_OBSERVER.to_string()),
                    GitlabUserObserver,
                    args.clone(),
                    myself.clone().into(),
                )
                .await
                {
                    warn!("failed to start users observer {e}")
                }
                if let Err(e) = Actor::spawn_linked(
                    Some(GITLAB_PROJECT_OBSERVER.to_string()),
                    GitlabProjectObserver,
                    args.clone(),
                    myself.clone().into(),
                )
                .await
                {
                    warn!("failed to start project observer {e}")
                }
                if let Err(e) = Actor::spawn_linked(
                    Some(GITLAB_PIPELINE_OBSERVER.to_string()),
                    GitlabPipelineObserver,
                    args.clone(),
                    myself.clone().into(),
                )
                .await
                {
                    warn!("failed to start project observer {e}")
                }
                if let Err(e) = Actor::spawn_linked(
                    Some(GITLAB_JOBS_OBSERVER.to_string()),
                    GitlabJobObserver,
                    args.clone(),
                    myself.clone().into(),
                )
                .await
                {
                    warn!("failed to start project observer {e}")
                }
                if let Err(e) = Actor::spawn_linked(
                    Some(GITLAB_GROUPS_OBSERVER.to_string()),
                    GitlabGroupObserver,
                    args.clone(),
                    myself.clone().into(),
                )
                .await
                {
                    warn!("failed to start group observer {e}")
                }
                if let Err(e) = Actor::spawn_linked(
                    Some(GITLAB_RUNNER_OBSERVER.to_string()),
                    GitlabRunnerObserver,
                    args.clone(),
                    myself.clone().into(),
                )
                .await
                {
                    warn!("failed to start runner observer {e}")
                }
                if let Err(e) = Actor::spawn_linked(
                    Some(GITLAB_REPOSITORY_OBSERVER.to_string()),
                    GitlabRepositoryObserver,
                    args.clone(),
                    myself.clone().into(),
                )
                .await
                {
                    warn!("failed to start runner observer {e}")
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
                // it could be caused by an invalid gitlab token being provided or any malformed query.
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
