//! src/bin/discover_repo.rs
//!
//! One-off harness: publishes a GitRepositoryDiscoveredEvent onto the
//! discovery topic so the git observer agent can be exercised end-to-end
//! without the gitlab processor / graphql scraper in the loop.
//!
//! Usage:
//!   cargo run --bin discover_repo -- https://github.com/cmu-sei/Polar.git
//!   cargo run --bin discover_repo -- https://example.org/r.git --synthetic
//!
//! With `--synthetic` it publishes no discovery. It publishes two commits and a ref update for the
//! repository on the events topic, as the observer would, so the processor and the graph can be
//! exercised without a repository to clone.
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
    synthetic: bool,
}

#[async_trait]
impl Actor for Harness {
    type Msg = HarnessMsg;
    type State = HarnessState;
    type Arguments = (String, bool); // http_url to publish, and whether to publish events instead

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        (http_url, synthetic): (String, bool),
    ) -> Result<Self::State, ActorProcessingErr> {
        let tcp_client = TcpClient::spawn(SERVICE_NAME, myself, |event| {
            Some(HarnessMsg::ClientEvent { event })
        })
        .await?;
        Ok(HarnessState {
            tcp_client,
            http_url,
            synthetic,
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
                ClientEvent::Registered { .. } if state.synthetic => {
                    use git_agent_common::{GitRepositoryMessage as M, publish_event};
                    let repo = polar::graph::nodes::git::RepoId::from_url(&state.http_url);
                    let commit = |oid: &str, parents: Vec<String>| M::CommitDiscovered {
                        repo: repo.clone(),
                        ref_name: "refs/heads/main".into(),
                        oid: oid.into(),
                        committer: "harness".into(),
                        time: 1,
                        message: format!("synthetic commit {oid}"),
                        parents,
                        observed_at: "1".into(),
                    };
                    let topic = polar::topics::GIT_REPOSITORY_EVENTS;
                    publish_event(&state.tcp_client, topic, &commit("aaaa", vec![]))?;
                    publish_event(&state.tcp_client, topic, &commit("bbbb", vec!["aaaa".into()]))?;
                    publish_event(
                        &state.tcp_client,
                        topic,
                        &M::RefUpdated {
                            repo,
                            ref_name: "refs/heads/main".into(),
                            old: None,
                            new: "bbbb".into(),
                            observed_at: "2".into(),
                        },
                    )?;
                    tracing::info!("published synthetic events for {}", state.http_url);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    myself.stop(None);
                }
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
                        share_identity: true,
                    })?;
                    tracing::info!("published discovery event for {}", state.http_url);
                    // The publish is queued to the client; stopping at once can end the process
                    // before it reaches the broker.
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
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
    let synthetic = std::env::args().nth(2).as_deref() == Some("--synthetic");
    let (_actor, handle) = Actor::spawn(None, Harness, (url, synthetic)).await?;
    handle.await?;
    Ok(())
}
