/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

//! src/bin/discover_repo.rs
//!
//! One-off harness: publishes a proposal that a repository be observed, as the Jira or GitLab consumer
//! would, so the git controller's admission rules can be exercised end-to-end without either of them in
//! the loop. The controller names the proposer by the certificate this process connects with.
//!
//! Usage:
//!   cargo run --bin git-discovery-harness -- https://github.com/cmu-sei/Polar.git
//!   cargo run --bin git-discovery-harness -- https://example.org/r.git --synthetic
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
use git_agent_common::proposal::{Evidence, PROPOSAL_OBSERVE_REPO_TOPIC, Proposal};
use polar::cassini::{CassiniClient, PublishRequest, TcpClient};
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
                    publish_event(
                        &state.tcp_client,
                        topic,
                        &commit("bbbb", vec!["aaaa".into()]),
                    )?;
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
                    let proposal = Proposal {
                        url: state.http_url.clone(),
                        evidence: Evidence {
                            source: "harness".into(),
                            reference: None,
                        },
                    };
                    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&proposal)?.to_vec();
                    state.tcp_client.publish(PublishRequest {
                        topic: PROPOSAL_OBSERVE_REPO_TOPIC.to_string(),
                        payload: bytes,
                        trace_ctx: None,
                        offline_behavior: OfflineBehavior::default(),
                        share_identity: true,
                    })?;
                    tracing::info!("proposed {}", state.http_url);
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

// Every Polar binary allocates with Scudo (docs/architecture/broker-baseline.md, "Decided").
#[global_allocator]
static GLOBAL: polar_scudo::Scudo = polar_scudo::Scudo;
