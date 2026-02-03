use crate::derive_instance_id;
use crate::groups::GitlabGroupObserver;
use crate::meta::MetaObserver;
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
use crate::META_OBSERVER;
use cassini_client::*;
use cassini_types::ClientEvent;
use polar::SupervisorMessage;
use ractor::async_trait;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::OutputPort;
use ractor::SupervisionEvent;
use std::env;
use tracing::debug;
use tracing::error;

pub struct ObserverSupervisor;

pub struct ObserverSupervisorState {
    pub tcp_client: ActorRef<TcpClientMessage>,
    pub gitlab_endpoint: String,
    pub gitlab_token: Option<String>,
    /// base interval that observers will use to query
    pub base_interval: u64,
    pub max_backoff_secs: u64,
    pub proxy_ca_cert_file: Option<String>,
}

#[async_trait]
impl Actor for ObserverSupervisor {
    type Msg = SupervisorMessage;
    type State = ObserverSupervisorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let gitlab_endpoint = url::Url::parse(env::var("GITLAB_ENDPOINT")?.as_str())?;
        let gitlab_token = env::var("GITLAB_TOKEN")?;

        // Helpful for looking at services behind a proxy
        let proxy_ca_cert_file = env::var("PROXY_CA_CERT").ok();

        let base_interval: u64 = env::var("OBSERVER_BASE_INTERVAL")
            .expect("Expected to read a value for OBSERVER_BASE_INTERVAL")
            .parse()
            .unwrap_or(300); // 5 minute default

        // define an output port for the actor to subscribe to
        let events_output = std::sync::Arc::new(OutputPort::default());
        // subscribe self to this port
        events_output.subscribe(myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        });

        // TODO: we don't make the observer respond to any configuration outputs or messages from the queue YET,
        // When we do, start a dispatcher, subscribe it to the queue_output, and go from there.
        //
        let client_config = TCPClientConfig::new()?;

        let (tcp_client, _) = Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config: client_config,
                registration_id: None,
                events_output,
            },
            myself.clone().into(),
        )
        .await?;

        Ok(ObserverSupervisorState {
            tcp_client,
            gitlab_endpoint: gitlab_endpoint.to_string(),
            gitlab_token: Some(gitlab_token),
            base_interval,
            max_backoff_secs: 6000, // TODO: make configurable, but this is ok for now
            proxy_ca_cert_file,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::ClientEvent { event } => {
                match event {
                    ClientEvent::Registered { registration_id } => {
                        // set up args for observer actors
                        let args = GitlabObserverArgs {
                            gitlab_endpoint: state.gitlab_endpoint.clone(),
                            instance_uid: derive_instance_id(&state.gitlab_endpoint),
                            token: state.gitlab_token.clone(),
                            registration_id: registration_id.clone(),
                            web_client: polar::get_web_client()?,
                            base_interval: state.base_interval,
                            max_backoff: state.max_backoff_secs,
                            tcp_client: state.tcp_client.clone(),
                        };

                        //TODO: Should other observers wait until the meta observer is online and observing?
                        //TODO: decide which observers to start based off of some configuration
                        Actor::spawn_linked(
                            Some(META_OBSERVER.to_string()),
                            MetaObserver,
                            args.clone(),
                            myself.clone().into(),
                        )
                        .await?;

                        Actor::spawn_linked(
                            Some(GITLAB_USERS_OBSERVER.to_string()),
                            GitlabUserObserver,
                            args.clone(),
                            myself.clone().into(),
                        )
                        .await?;

                        Actor::spawn_linked(
                            Some(GITLAB_PROJECT_OBSERVER.to_string()),
                            GitlabProjectObserver,
                            args.clone(),
                            myself.clone().into(),
                        )
                        .await?;

                        Actor::spawn_linked(
                            Some(GITLAB_PIPELINE_OBSERVER.to_string()),
                            GitlabPipelineObserver,
                            args.clone(),
                            myself.clone().into(),
                        )
                        .await?;

                        Actor::spawn_linked(
                            Some(GITLAB_JOBS_OBSERVER.to_string()),
                            GitlabJobObserver,
                            args.clone(),
                            myself.clone().into(),
                        )
                        .await?;

                        Actor::spawn_linked(
                            Some(GITLAB_GROUPS_OBSERVER.to_string()),
                            GitlabGroupObserver,
                            args.clone(),
                            myself.clone().into(),
                        )
                        .await?;

                        Actor::spawn_linked(
                            Some(GITLAB_RUNNER_OBSERVER.to_string()),
                            GitlabRunnerObserver,
                            args.clone(),
                            myself.clone().into(),
                        )
                        .await?;

                        Actor::spawn_linked(
                            Some(GITLAB_REPOSITORY_OBSERVER.to_string()),
                            GitlabRepositoryObserver,
                            args.clone(),
                            myself.clone().into(),
                        )
                        .await?;
                    }
                    ClientEvent::MessagePublished { .. } => {
                        todo!("Deserialize and dispatch message from the queue");
                        // TODO: We haven't yet figured out what kind of messages observer supervisors will receive yet, but when we do
                        // this is what we'll call
                        // ObserverSupervisor::deserialize_and_dispatch(topic, payload)
                    }
                    ClientEvent::TransportError { .. } => {
                        todo!("Handle client transport error")
                    }
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
