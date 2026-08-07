//! src/bin/discover_repo.rs
//!
//! One-off harness: publishes a GitRepositoryDiscoveredEvent onto the
//! discovery topic so the git observer agent can be exercised end-to-end
//! without the gitlab processor / graphql scraper in the loop.
//!
//! Usage:
//!   cargo run --bin discover_repo -- https://github.com/cmu-sei/Polar.git
//!
//! The target agent must already be running and past its init() (i.e. it
//! has spawned its repo supervisor and subscribed to the discovery topic)
//! before you run this -- see notes below.

use cassini_client::OfflineBehavior;
use cassini_types::ClientEvent;
use polar::{
    cassini::{CassiniClient, PublishRequest, TcpClient},
    events::GitRepositoryDiscoveredEvent,
    topics::GIT_REPOSITORY_DISCOVERED,
};
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};

const SERVICE_NAME: &str = "git-discovery-harness";

struct Harness;

enum HarnessMsg {
    ClientEvent { event: ClientEvent },
}

struct HarnessState {
    tcp_client: TcpClient,
    http_url: String,
}

#[async_trait]
impl Actor for Harness {
    type Msg = HarnessMsg;
    type State = HarnessState;
    type Arguments = String; // http_url to publish

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        http_url: String,
    ) -> Result<Self::State, ActorProcessingErr> {
        let tcp_client = TcpClient::spawn(SERVICE_NAME, myself, |event| {
            Some(HarnessMsg::ClientEvent { event })
        })
        .await?;
        Ok(HarnessState {
            tcp_client,
            http_url,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            HarnessMsg::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let ev = GitRepositoryDiscoveredEvent {
                        http_url: Some(state.http_url.clone()),
                        ssh_url: None,
                    };
                    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&ev)?.to_vec();
                    state.tcp_client.publish(PublishRequest {
                        topic: GIT_REPOSITORY_DISCOVERED.to_string(),
                        payload: bytes,
                        trace_ctx: None,
                        offline_behavior: OfflineBehavior::default(),
                    })?;
                    tracing::info!("published discovery event for {}", state.http_url);
                    myself.stop(None);
                }
                ClientEvent::TransportError { reason } => {
                    myself.stop(Some(reason));
                }
                _ => {}
            },
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    polar::init_logging(SERVICE_NAME.to_string());
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://github.com/cmu-sei/Polar.git".to_string());
    let (_actor, handle) = Actor::spawn(None, Harness, url).await?;
    handle.await?;
    Ok(())
}
