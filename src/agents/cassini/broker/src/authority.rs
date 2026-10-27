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

//! Who may publish and subscribe to what (docs/architecture/policy-enforcement.md, stage 5b). The broker cannot be
//! a client of itself, so it keeps the credentials agent's authority the way every agent does, with an
//! `AuthorityHandle` of its own, and hears the authority topics as they pass through it, with the publisher's
//! identity from its own Listener. Each publish and subscribe is decided by the rule every agent's own gate applies
//! (`polar_agent_builder::topic_gate::rule`): Cedar first, then the floor the client's certificate gives, and
//! refused by default.

use cassini_types::BrokerMessage;
use credentials_agent_common::authority_update::{
    AUTHORITY_SNAPSHOT_REPLIES_TOPIC, AUTHORITY_SNAPSHOT_REQUESTS_TOPIC, AUTHORITY_UPDATES_TOPIC,
    AuthoritySnapshot, AuthoritySnapshotRequest, AuthorityUpdate,
};
use polar_agent_builder::authority::{AuthorityChanged, AuthorityHandle};
use polar_agent_builder::topic_gate::{ask, rule};
use polar_agent_builder::{Access, Floor, PolicyLookup};
use ractor::{Actor, ActorCell, ActorProcessingErr, ActorRef};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// The name the broker's policy and permissions actors are registered under (`cassini.policy`).
pub const SERVICE: &str = "cassini";

/// The registration id the broker publishes its own requests under: no client has it.
const OWN_REGISTRATION: &str = "cassini";

/// The broker's authority, shared by every Listener.
#[derive(Clone)]
pub struct BrokerAuthority {
    handle: AuthorityHandle,
    lookup: PolicyLookup,
    /// How many times the authority has changed: a decision made under an earlier count is stale.
    changes: Arc<AtomicU64>,
    /// How long a Listener keeps a decision; zero keeps none.
    cache_for: Duration,
}

impl BrokerAuthority {
    /// Starts the broker's authority under `supervisor`, believing only `publisher` (the credentials agent's
    /// identity). What it wants to ask for is published on the requests topic through `topic_mgr`.
    /// The credentials agent's key is believed under a certificate from `pinned_ca_file`, the CA the broker verifies its
    /// clients under (docs/architecture/authority-distribution.md, "Pinning the key to the CA"); without a readable one,
    /// nothing is believed.
    pub async fn spawn(
        publisher: String,
        pinned_ca_file: &str,
        supervisor: ActorCell,
        topic_mgr: ActorRef<BrokerMessage>,
        cache_for: Duration,
    ) -> Result<Self, ActorProcessingErr> {
        let trust = match polar_agent_builder::signed::trust_from_file(pinned_ca_file) {
            Ok(trust) => trust,
            Err(why) => {
                warn!("no authority from the credentials agent will be believed: {why}");
                polar_signing::Trust::from_pem(&[]).expect("no anchors is a trust")
            }
        };
        let handle = AuthorityHandle::spawn(SERVICE, publisher, trust, supervisor.clone()).await?;
        let changes = Arc::new(AtomicU64::new(0));
        let (counter, _) =
            Actor::spawn_linked(None, ChangeCounter, changes.clone(), supervisor).await?;
        handle.changes().subscribe(counter, Some);
        let request: Arc<Vec<u8>> = Arc::new(
            rkyv::to_bytes::<rkyv::rancor::Error>(&AuthoritySnapshotRequest)
                .map_err(|e| ActorProcessingErr::from(e.to_string()))?
                .to_vec(),
        );
        handle.asks().subscribe(topic_mgr, move |_| {
            Some(BrokerMessage::PublishRequest {
                registration_id: OWN_REGISTRATION.to_string(),
                topic: AUTHORITY_SNAPSHOT_REQUESTS_TOPIC.to_string(),
                payload: request.clone(),
                reply_to: None,
                trace_ctx: None,
                published_by: None,
            })
        });
        // Asked once at start: the broker is not a client, so no registration asks for it.
        handle.registered();
        Ok(Self {
            handle,
            lookup: PolicyLookup::for_service(SERVICE),
            changes,
            cache_for,
        })
    }

    /// How many times the authority has changed so far.
    pub fn changes(&self) -> u64 {
        self.changes.load(Ordering::Acquire)
    }

    /// An empty cache for one connection's decisions.
    pub fn cache(&self) -> DecisionCache {
        DecisionCache::new(self.cache_for)
    }

    /// Whether the client whose floor is `floor` may use `topic` this way; `Err` says why not. The agent's own gate
    /// asks the same question with the same rule, so a refusal here means one of them holds stale authority.
    pub async fn decide(&self, floor: &Floor, topic: &str, access: Access) -> Result<(), String> {
        let said = ask(
            &self.lookup,
            floor.identity(),
            Some(floor.certificate_pem().to_string()),
            topic,
            access,
            floor.facts(topic, access),
        )
        .await;
        rule(&said, floor.allows(topic, access), topic, access)
            .map_err(|why| format!("the broker refused: {why}"))
    }

    /// Hands a publish on an authority topic to the broker's authority, as published by `publisher`, the identity
    /// the broker verified. Only the credentials agent is believed, which the authority checks.
    pub fn hear(&self, topic: &str, payload: &[u8], publisher: &str) {
        let publisher = Some(publisher.to_string());
        match topic {
            AUTHORITY_UPDATES_TOPIC => {
                match rkyv::from_bytes::<AuthorityUpdate, rkyv::rancor::Error>(payload) {
                    Ok(update) => self.handle.update(update, publisher),
                    Err(e) => {
                        warn!("a message on the authority updates topic is not an update: {e}")
                    }
                }
            }
            AUTHORITY_SNAPSHOT_REPLIES_TOPIC => {
                match rkyv::from_bytes::<AuthoritySnapshot, rkyv::rancor::Error>(payload) {
                    Ok(snapshot) => self.handle.snapshot(snapshot, publisher),
                    Err(e) => {
                        warn!("a message on the authority snapshot topic is not a snapshot: {e}")
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether `topic` carries authority the broker keeps.
    pub fn carries_authority(topic: &str) -> bool {
        topic == AUTHORITY_UPDATES_TOPIC || topic == AUTHORITY_SNAPSHOT_REPLIES_TOPIC
    }

    pub fn handle(&self) -> &AuthorityHandle {
        &self.handle
    }
}

/// What a watcher session is told when it asks for anything but authority (docs/architecture/key-custody.md, step 5).
pub const WATCHER_REFUSAL: &str = "the broker refused: a watcher session may hear the credentials agent's authority and ask for it, nothing else";

/// What a revoked certificate is told when it asks to register as anything but a watcher.
pub const REVOKED_REFUSAL: &str =
    "the broker refused: this certificate is revoked, and may register only a watcher session";

/// Whether a watcher session may use `topic` this way: hear the credentials agent's updates and snapshots, and ask for
/// a snapshot. Within these it is decided as its identity's session is, so a watcher never may more. Pure.
pub fn watcher_may(topic: &str, access: Access) -> bool {
    matches!(
        (topic, access),
        (AUTHORITY_UPDATES_TOPIC, Access::Subscribe)
            | (AUTHORITY_SNAPSHOT_REPLIES_TOPIC, Access::Subscribe)
            | (AUTHORITY_SNAPSHOT_REQUESTS_TOPIC, Access::Publish)
    )
}

/// Counts the changes of authority, so that every Listener's cached decisions go stale at once without a lock or a
/// message to each (docs/architecture/policy-enforcement.md, "Decisions": the cache is emptied on a change).
struct ChangeCounter;

#[ractor::async_trait]
impl Actor for ChangeCounter {
    type Msg = AuthorityChanged;
    type State = Arc<AtomicU64>;
    type Arguments = Arc<AtomicU64>;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        changes: Arc<AtomicU64>,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(changes)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        changed: AuthorityChanged,
        changes: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let count = changes.fetch_add(1, Ordering::AcqRel) + 1;
        info!(kind = ?changed.kind, version = changed.version, count, "authority changed: every cached decision is stale");
        // Counted first, so the subscriptions are judged under the new authority, not from a kept decision.
        if let Some(listeners) =
            ractor::registry::where_is(crate::LISTENER_MANAGER_NAME.to_string())
        {
            let listeners: ActorRef<BrokerMessage> = listeners.into();
            let _ = listeners.send_message(BrokerMessage::JudgeSubscriptions { topics: None });
        }
        Ok(())
    }
}

/// The most decisions one connection keeps; past it they are all dropped and made again, so a client that uses
/// endless topics cannot make the broker hold endless answers.
pub const MOST_KEPT: usize = 1024;

/// One connection's decisions, each kept for a short time under the authority it was made with: the broker is
/// asked about every publish, and a decision otherwise costs a call to the policy actor and an evaluation. An entry
/// made under an earlier count of changes, or older than its time, is a miss. The time bounds how late a rule that
/// reads the clock can take effect.
pub struct DecisionCache {
    ttl: Duration,
    entries: HashMap<(String, Access), Kept>,
}

struct Kept {
    decided: Result<(), String>,
    at: Instant,
    changes: u64,
}

impl DecisionCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: HashMap::new(),
        }
    }

    /// The decision kept for `topic`, used this way, if it is still good at `now` with the authority changed
    /// `changes` times.
    pub fn get(
        &self,
        topic: &str,
        access: Access,
        now: Instant,
        changes: u64,
    ) -> Option<Result<(), String>> {
        let kept = self.entries.get(&(topic.to_string(), access))?;
        (kept.changes == changes && now.saturating_duration_since(kept.at) < self.ttl)
            .then(|| kept.decided.clone())
    }

    /// Keeps `decided`, made at `now` with the authority changed `changes` times (read before deciding, so a change
    /// while it was being made leaves it stale).
    pub fn put(
        &mut self,
        topic: &str,
        access: Access,
        decided: Result<(), String>,
        now: Instant,
        changes: u64,
    ) {
        if self.ttl.is_zero() {
            return;
        }
        if self.entries.len() >= MOST_KEPT {
            self.entries.clear();
        }
        self.entries.insert(
            (topic.to_string(), access),
            Kept {
                decided,
                at: now,
                changes,
            },
        );
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::time::{Duration, Instant};

    const TTL: Duration = Duration::from_secs(5);

    #[test]
    fn a_decision_is_kept_for_its_time_under_the_authority_it_was_made_with() {
        let mut cache = DecisionCache::new(TTL);
        let at = Instant::now();
        assert_eq!(cache.get("news", Access::Publish, at, 0), None);
        cache.put("news", Access::Publish, Ok(()), at, 0);
        assert_eq!(
            cache.get("news", Access::Publish, at + TTL / 2, 0),
            Some(Ok(()))
        );
        // Only for that use of the topic.
        assert_eq!(cache.get("news", Access::Subscribe, at, 0), None);
        // Not past its time, and not once the authority has changed.
        assert_eq!(cache.get("news", Access::Publish, at + TTL, 0), None);
        assert_eq!(cache.get("news", Access::Publish, at, 1), None);
    }

    #[test]
    fn a_refusal_is_kept_as_a_permission_is() {
        let mut cache = DecisionCache::new(TTL);
        let at = Instant::now();
        cache.put("news", Access::Publish, Err("no".into()), at, 3);
        assert_eq!(
            cache.get("news", Access::Publish, at, 3),
            Some(Err("no".into()))
        );
    }

    #[test]
    fn with_no_time_to_keep_nothing_is_kept() {
        let mut cache = DecisionCache::new(Duration::ZERO);
        let at = Instant::now();
        cache.put("news", Access::Publish, Ok(()), at, 0);
        assert_eq!(cache.get("news", Access::Publish, at, 0), None);
    }

    #[test]
    fn a_connection_that_uses_too_many_topics_starts_again_rather_than_growing() {
        let mut cache = DecisionCache::new(TTL);
        let at = Instant::now();
        for n in 0..MOST_KEPT + 10 {
            cache.put(&format!("topic-{n}"), Access::Publish, Ok(()), at, 0);
        }
        assert!(cache.len() <= MOST_KEPT);
    }

    #[derive(Debug, Clone)]
    enum Step {
        Put {
            topic: u8,
            publish: bool,
            allowed: bool,
        },
        Wait(u64),
        Change,
    }

    fn step() -> impl Strategy<Value = Step> {
        prop_oneof![
            (0u8..3, any::<bool>(), any::<bool>()).prop_map(|(topic, publish, allowed)| {
                Step::Put {
                    topic,
                    publish,
                    allowed,
                }
            }),
            (0u64..4000).prop_map(Step::Wait),
            Just(Step::Change),
        ]
    }

    proptest! {
        /// Whatever happens, an answer is the last one put for that topic and use, made under the current authority
        /// and less than its time ago; anything else is a miss.
        #[test]
        fn no_answer_outlives_its_time_or_a_change_of_authority(steps in proptest::collection::vec(step(), 0..40)) {
            let mut cache = DecisionCache::new(Duration::from_millis(2500));
            let start = Instant::now();
            let mut now = start;
            let mut epoch = 0u64;
            // The last put of each key, with when and under which authority.
            let mut last: HashMap<(u8, bool), (bool, Instant, u64)> = HashMap::new();
            for step in steps {
                match step {
                    Step::Put { topic, publish, allowed } => {
                        let access = if publish { Access::Publish } else { Access::Subscribe };
                        let decided = if allowed { Ok(()) } else { Err("no".to_string()) };
                        cache.put(&format!("t{topic}"), access, decided, now, epoch);
                        last.insert((topic, publish), (allowed, now, epoch));
                    }
                    Step::Wait(ms) => now += Duration::from_millis(ms),
                    Step::Change => epoch += 1,
                }
                for topic in 0u8..3 {
                    for publish in [true, false] {
                        let access = if publish { Access::Publish } else { Access::Subscribe };
                        let got = cache.get(&format!("t{topic}"), access, now, epoch);
                        let expected = last.get(&(topic, publish)).and_then(|(allowed, at, made)| {
                            (*made == epoch && now.duration_since(*at) < Duration::from_millis(2500))
                                .then(|| if *allowed { Ok(()) } else { Err("no".to_string()) })
                        });
                        prop_assert_eq!(got, expected);
                    }
                }
            }
        }
    }
}
