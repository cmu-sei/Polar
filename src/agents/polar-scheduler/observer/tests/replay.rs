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

//! An agent that has just started asks for the signed configuration held for it, and the watcher answers
//! from the checkout: the repository as last pulled, not anything remembered anywhere else.

use cassini_client::TcpClientMessage;
use polar_scheduler_common::{ConfigurationNotification, SCHEDULER_IN_TOPIC, configuration_topic};
use polar_scheduler_observer::watcher::{GitWatcherActor, GitWatcherMsg};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Published = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

struct Cassini(Published);

#[ractor::async_trait]
impl Actor for Cassini {
    type Msg = TcpClientMessage;
    type State = Published;
    type Arguments = ();
    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(self.0.clone())
    }
    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if let TcpClientMessage::Publish { topic, payload, .. } = message {
            state.lock().unwrap().push((topic, payload));
        }
        Ok(())
    }
}

/// A checkout with two signed documents, as Dhall records the way an author writes them.
fn checkout() -> tempfile::TempDir {
    // Not a dot-named directory: the scan skips those, as it skips .git.
    let dir = tempfile::Builder::new()
        .prefix("checkout")
        .tempdir()
        .unwrap();
    std::fs::create_dir_all(dir.path().join("permanent")).unwrap();
    for (file, id, agent, version) in [
        ("a.dhall", "item-a", "agent-a", 2),
        ("b.dhall", "item-b", "agent-b", 1),
    ] {
        std::fs::write(
            dir.path().join("permanent").join(file),
            format!(
                r#"{{ payload = {{ type = "configuration", id = "{id}", target = {{ agent = "{agent}" }}, version = {version}, changes = [] : List {{ name : Text, value : Text }} }}
                   , signature = {{ algorithm = "ed25519", value = "AAAA", certificate_chain = [ "x" ] }} }}"#
            ),
        )
        .unwrap();
    }
    dir
}

struct Rig {
    watcher: ActorRef<GitWatcherMsg>,
    published: Published,
    _dir: tempfile::TempDir,
}

impl Rig {
    async fn new() -> Self {
        let dir = checkout();
        let published = Arc::new(Mutex::new(Vec::new()));
        let (cassini, _) = Actor::spawn(None, Cassini(published.clone()), ())
            .await
            .unwrap();
        let (watcher, _) = Actor::spawn(
            None,
            GitWatcherActor,
            (dir.path().to_string_lossy().into_owned(), cassini),
        )
        .await
        .unwrap();
        Self {
            watcher,
            published,
            _dir: dir,
        }
    }

    fn replay(&self, agent: &str) {
        self.watcher
            .cast(GitWatcherMsg::Replay {
                agent: agent.to_string(),
            })
            .unwrap();
    }

    /// What was sent to agents: the notifications on their configuration topics.
    fn sent_to(&self, agent: &str) -> Vec<ConfigurationNotification> {
        self.published
            .lock()
            .unwrap()
            .iter()
            .filter(|(topic, _)| *topic == configuration_topic(agent))
            .map(|(_, payload)| {
                rkyv::from_bytes::<ConfigurationNotification, rkyv::rancor::Error>(payload).unwrap()
            })
            .collect()
    }
}

async fn settle() {
    tokio::time::sleep(Duration::from_secs(2)).await;
}

#[tokio::test]
async fn an_agent_is_sent_the_documents_in_the_checkout_that_name_it_and_no_others() {
    let rig = Rig::new().await;
    rig.watcher.cast(GitWatcherMsg::PerformInitialScan).unwrap();
    settle().await;
    rig.replay("agent-a");
    settle().await;

    let sent = rig.sent_to("agent-a");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].agent, "agent-a");
    let document: serde_json::Value = serde_json::from_str(&sent[0].document_json).unwrap();
    assert_eq!(document["payload"]["id"], "item-a");
    assert_eq!(document["payload"]["version"], 2);
    assert!(
        rig.sent_to("agent-b").is_empty(),
        "agent-b did not ask, so it is sent nothing"
    );
    rig.replay("agent-z");
    settle().await;
    assert!(rig.sent_to("agent-z").is_empty(), "nothing is held for it");
}

#[tokio::test]
async fn a_request_that_arrives_before_the_scan_has_finished_is_answered_when_it_has() {
    let rig = Rig::new().await;
    // Asked first: the checkout has not been read yet, so there is nothing true to say. It is
    // answered once, after the scan, not answered empty and not answered twice.
    rig.replay("agent-a");
    rig.watcher.cast(GitWatcherMsg::PerformInitialScan).unwrap();
    settle().await;
    assert_eq!(rig.sent_to("agent-a").len(), 1);

    // And the scan's own announcements still went out as before.
    let announced = rig
        .published
        .lock()
        .unwrap()
        .iter()
        .filter(|(topic, _)| topic == SCHEDULER_IN_TOPIC)
        .count();
    assert_eq!(announced, 2);
}
