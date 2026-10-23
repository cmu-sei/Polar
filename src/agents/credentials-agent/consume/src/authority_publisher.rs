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

//! The credentials agent's side of authority distribution (docs/architecture/authority-distribution.md): when
//! its policy, the group membership or the set of revoked certificates changes, it signs the new state, numbers
//! it through the ledger, publishes it, and applies it to its own policy and permissions actors the same way
//! every other agent will.
//!
//! It also answers an agent that asks for the current authority (with the key and the latest update of each
//! kind, published once for everyone, never more than once in five seconds), and republishes that every half
//! hour of quiet, the timer starting over whenever an update goes out: changes set the cadence, and the
//! half-hour republish is only the backstop for an agent that missed one.
//!
//! It is told what changed, not asked to look: the supervisor reads a runtime section and passes its value in,
//! so this has no dependency on where the sections come from.

use crate::ledger::LedgerMessage;
use credentials_agent_common::authority_book::AuthorityBook;
use credentials_agent_common::authority_update::{
    AUTHORITY_SNAPSHOT_REPLIES_TOPIC, AUTHORITY_UPDATES_TOPIC, AuthorityBody, AuthorityKind,
};
use polar_agent_builder::authority::AuthorityHandle;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use rsa::RsaPrivateKey;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

/// How long of quiet before the current authority is published again.
pub const REPUBLISH_AFTER: Duration = Duration::from_secs(30 * 60);
/// The least time between two publications of the snapshot in answer to requests: it goes to everyone, so one
/// serves all who asked.
pub const SNAPSHOT_COALESCE: Duration = Duration::from_secs(5);

/// Puts bytes on a Cassini topic.
pub type Publish = Arc<dyn Fn(&str, Vec<u8>) + Send + Sync>;

pub enum PublisherMessage {
    /// Pin the key locally, and say what is known.
    Start,
    /// A runtime section (`policy` or `groups`) was replaced.
    SectionChanged {
        section: String,
        version: u64,
        body: Value,
        authored_by: Option<String>,
    },
    /// The set of revoked serials grew.
    RevocationsChanged,
    /// Someone asked for the current authority.
    SnapshotRequested,
    /// The quiet period may be over (internal; ignored if an update went out since it was set).
    Republish { generation: u64 },
}

pub struct AuthorityPublisher;

pub struct PublisherArgs {
    pub ledger: ActorRef<LedgerMessage>,
    pub key: RsaPrivateKey,
    pub publish: Publish,
    /// This agent's own policy and permissions actors, kept current by what it publishes.
    pub local: AuthorityHandle,
    /// The identity the broker gives this agent, which its own authority actor listens to.
    pub own_identity: String,
    /// What the judge accepted of each document: its body as it is put in force, and what the model judged it to do,
    /// recorded with its update.
    pub acceptances: crate::authority_judge::Acceptances,
}

pub struct PublisherState {
    args: PublisherArgs,
    book: AuthorityBook,
    last_snapshot: Option<Instant>,
    generation: u64,
}

impl PublisherState {
    fn publish_bytes(&self, topic: &str, bytes: Vec<u8>) {
        (self.args.publish)(topic, bytes);
    }

    /// The key and the latest update of each kind, to everyone who is listening.
    fn publish_snapshot(&mut self) {
        match self.book.snapshot() {
            Ok(snapshot) => match rkyv::to_bytes::<rkyv::rancor::Error>(&snapshot) {
                Ok(bytes) => self.publish_bytes(AUTHORITY_SNAPSHOT_REPLIES_TOPIC, bytes.to_vec()),
                Err(e) => error!("cannot encode the authority snapshot: {e}"),
            },
            Err(e) => error!("cannot build the authority snapshot: {e}"),
        }
    }

    /// Starts the quiet period over: after half an hour with nothing published, the current authority is
    /// published again.
    fn restart_the_wait(&mut self, myself: &ActorRef<PublisherMessage>) {
        self.generation += 1;
        let generation = self.generation;
        drop(
            myself.send_after(REPUBLISH_AFTER, move || PublisherMessage::Republish {
                generation,
            }),
        );
    }

    /// Signs, numbers, publishes and applies one update, if it would replace what is held.
    async fn publish(
        &mut self,
        myself: &ActorRef<PublisherMessage>,
        body: AuthorityBody,
        version: u64,
        authored_by: Option<String>,
        effect: Option<credentials_agent_common::events::AuthorityEffect>,
    ) {
        let kind = body.kind();
        if !self.book.would_replace(kind, version) {
            debug!(
                ?kind,
                version, "not published: it would not replace what is held"
            );
            return;
        }
        let name = match kind {
            AuthorityKind::Policy => "policy",
            AuthorityKind::Groups => "groups",
            AuthorityKind::Revoked => "revoked",
        };
        let sequence =
            match ractor::call!(self.args.ledger, |reply| LedgerMessage::PublishAuthority {
                kind: name.to_string(),
                version,
                effect,
                reply,
            }) {
                Ok(Ok(sequence)) => sequence,
                Ok(Err(e)) => return error!("the ledger would not number the update: {e}"),
                Err(e) => return error!("the ledger did not answer: {e}"),
            };
        let update = match self.book.publish(body, version, sequence, authored_by) {
            Ok(update) => update,
            Err(e) => return error!("could not sign the update: {e}"),
        };
        info!(
            kind = name,
            version, sequence, "publishing an authority update"
        );
        // Here first, the way every other agent will hear it, so this agent does not depend on a round
        // trip through the broker to enforce what it has just decided.
        self.args
            .local
            .update(update.clone(), Some(self.args.own_identity.clone()));
        match rkyv::to_bytes::<rkyv::rancor::Error>(&update) {
            Ok(bytes) => self.publish_bytes(AUTHORITY_UPDATES_TOPIC, bytes.to_vec()),
            Err(e) => error!("cannot encode the authority update: {e}"),
        }
        self.restart_the_wait(myself);
    }
}

/// What a runtime section says, as the body of an update. A section that is not one of the two, or does not
/// read as it should, is not published: the credentials agent validates a section when it receives it, so
/// this is a second line.
pub(crate) fn body_of(section: &str, body: &Value) -> Result<AuthorityBody, String> {
    match section {
        "policy" => body
            .get("source")
            .and_then(Value::as_str)
            .map(|source| AuthorityBody::Policy {
                source: source.to_string(),
            })
            .ok_or_else(|| "the policy section has no text `source`".to_string()),
        "groups" => {
            let memberships: BTreeMap<String, Vec<String>> = body
                .get("memberships")
                .cloned()
                .ok_or_else(|| "the groups section has no `memberships`".to_string())
                .and_then(|m| serde_json::from_value(m).map_err(|e| e.to_string()))?;
            let hierarchy: cert_issuer_common::hierarchy::Hierarchy =
                serde_json::from_value(serde_json::json!({
                    "groups": body.get("groups").cloned().unwrap_or(serde_json::json!({})),
                    "roles": body.get("roles").cloned().unwrap_or(serde_json::json!({})),
                }))
                .map_err(|e| e.to_string())?;
            Ok(AuthorityBody::Groups {
                memberships,
                hierarchy,
            })
        }
        other => Err(format!("`{other}` is not a section that is published")),
    }
}

#[ractor::async_trait]
impl Actor for AuthorityPublisher {
    type Msg = PublisherMessage;
    type State = PublisherState;
    type Arguments = PublisherArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: PublisherArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        // The numbering goes on from where the ledger left it, so an agent that holds the last number it
        // saw is not confused by a restart.
        let last_sequence = ractor::call!(args.ledger, LedgerMessage::AuthoritySequence)
            .map_err(|e| ActorProcessingErr::from(format!("the ledger did not answer: {e}")))?;
        Ok(PublisherState {
            book: AuthorityBook::new(args.key.clone(), last_sequence),
            args,
            last_snapshot: None,
            generation: 0,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            PublisherMessage::Start => {
                // This agent's own authority actor listens to its own identity: give it the key, then say
                // what is known (the revoked serials the ledger holds; the sections arrive as they replay).
                match state.book.snapshot() {
                    Ok(snapshot) => state
                        .args
                        .local
                        .snapshot(snapshot, Some(state.args.own_identity.clone())),
                    Err(e) => error!("cannot build the authority snapshot: {e}"),
                }
                state.publish_snapshot();
                state.restart_the_wait(&myself);
                myself.cast(PublisherMessage::RevocationsChanged)?;
            }
            PublisherMessage::SectionChanged {
                section,
                version,
                body,
                authored_by,
            } => {
                // What the judge accepted, as it put it in force; a section no judge saw (one held when this agent
                // started) is put in force here the same way.
                let accepted = match state.args.acceptances.take(&section, version) {
                    Some(accepted) => Ok((accepted.body, accepted.effect)),
                    None => match crate::authority_judge::account_names(&state.args.ledger).await {
                        Ok(names) => crate::authority_judge::in_force_body(&section, &body, &names)
                            .map(|body| (body, None))
                            .map_err(|reasons| reasons.join("; ")),
                        Err(e) => Err(e),
                    },
                };
                match accepted {
                    Ok((body, effect)) => {
                        state
                            .publish(&myself, body, version, authored_by, effect)
                            .await
                    }
                    Err(e) => warn!(%section, version, "not published: {e}"),
                }
            }
            PublisherMessage::RevocationsChanged => {
                let serials = ractor::call!(state.args.ledger, LedgerMessage::RevokedSerials)
                    .map_err(|e| {
                        ActorProcessingErr::from(format!("the ledger did not answer: {e}"))
                    })?;
                // The set only grows, so how many there are says which version it is; none is nothing to say.
                if !serials.is_empty() {
                    let version = serials.len() as u64;
                    state
                        .publish(
                            &myself,
                            AuthorityBody::Revoked { serials },
                            version,
                            None,
                            None,
                        )
                        .await;
                }
            }
            PublisherMessage::SnapshotRequested => {
                let now = Instant::now();
                if state
                    .last_snapshot
                    .is_some_and(|at| now.duration_since(at) < SNAPSHOT_COALESCE)
                {
                    debug!("a snapshot went out a moment ago and serves this request too");
                } else {
                    state.last_snapshot = Some(now);
                    state.publish_snapshot();
                }
            }
            PublisherMessage::Republish { generation } => {
                if generation == state.generation {
                    info!("republishing the current authority after a quiet half hour");
                    state.last_snapshot = Some(Instant::now());
                    state.publish_snapshot();
                    state.restart_the_wait(&myself);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_groups_section_s_arrangement_is_carried_into_the_update() {
        let body = serde_json::json!({
            "memberships": { "alice": ["operators"] },
            "groups": { "operators": { "roles": ["policy-author"] } },
            "roles": { "policy-author": { "capabilities": ["update-policy"] } }
        });
        let AuthorityBody::Groups {
            memberships,
            hierarchy,
        } = body_of("groups", &body).unwrap()
        else {
            panic!("not a groups update");
        };
        assert_eq!(memberships["alice"], ["operators"]);
        assert_eq!(hierarchy.groups["operators"].roles, ["policy-author"]);
        assert_eq!(
            hierarchy.roles["policy-author"].capabilities,
            ["update-policy"]
        );
        // One with no arrangement carries an empty one.
        let AuthorityBody::Groups { hierarchy, .. } =
            body_of("groups", &serde_json::json!({ "memberships": {} })).unwrap()
        else {
            panic!("not a groups update");
        };
        assert!(hierarchy.is_empty());
    }
    use crate::ledger::{LedgerActor, LedgerActorArgs};
    use credentials_agent_common::authority_update::{AuthoritySnapshot, AuthorityUpdate, verify};
    use credentials_agent_common::events::{CredentialsEvent, RevocationReason};
    use polar_agent_builder::authority::{AuthorityCore, Effect};
    use serde_json::json;
    use std::sync::Mutex;

    const OWN: &str = "credentials-agent.polar.serviceaccount.cluster.local";

    /// Each message the publisher publishes: its topic and payload.
    type Published = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

    struct Rig {
        publisher: ActorRef<PublisherMessage>,
        ledger: ActorRef<LedgerMessage>,
        local: AuthorityHandle,
        published: Published,
        acceptances: crate::authority_judge::Acceptances,
        path: String,
        _dir: tempfile::TempDir,
    }

    /// Actor names are process-wide, and the tests run side by side.
    fn unique_service() -> String {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        format!(
            "publisher-test-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        )
    }

    async fn ledger_at(path: &str) -> ActorRef<LedgerMessage> {
        let (ledger, _) = Actor::spawn(
            None,
            LedgerActor,
            LedgerActorArgs {
                log_path: path.to_string(),
                projection_actor: crate::projection::tests::fake_projection_actor().await,
            },
        )
        .await
        .unwrap();
        ledger
    }

    async fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("events.jsonl")
            .to_str()
            .unwrap()
            .to_string();
        let rig = rig_at(&path).await;
        // alice, whom the tests' documents name, has an account.
        crate::ledger::tests::holds_account(&rig.ledger, "alice", ALICE).await;
        Rig { _dir: dir, ..rig }
    }

    async fn rig_at(path: &str) -> Rig {
        struct Parent;
        #[ractor::async_trait]
        impl Actor for Parent {
            type Msg = ();
            type State = ();
            type Arguments = ();
            async fn pre_start(&self, _: ActorRef<()>, _: ()) -> Result<(), ActorProcessingErr> {
                Ok(())
            }
        }
        let ledger = ledger_at(path).await;
        let (parent, _) = Actor::spawn(None, Parent, ()).await.unwrap();
        let local = AuthorityHandle::spawn(&unique_service(), OWN, parent.get_cell())
            .await
            .unwrap();
        let published = Arc::new(Mutex::new(Vec::new()));
        let sink = published.clone();
        let acceptances = crate::authority_judge::Acceptances::default();
        let (publisher, _) = Actor::spawn(
            None,
            AuthorityPublisher,
            PublisherArgs {
                ledger: ledger.clone(),
                key: crate::escrow::tests::test_keypair().private_key().clone(),
                publish: Arc::new(move |topic, bytes| {
                    sink.lock().unwrap().push((topic.to_string(), bytes))
                }),
                local: local.clone(),
                own_identity: OWN.to_string(),
                acceptances: acceptances.clone(),
            },
        )
        .await
        .unwrap();
        publisher.cast(PublisherMessage::Start).unwrap();
        Rig {
            publisher,
            ledger,
            local,
            published,
            acceptances,
            path: path.to_string(),
            _dir: tempfile::tempdir().unwrap(),
        }
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    impl Rig {
        fn updates(&self) -> Vec<AuthorityUpdate> {
            self.published
                .lock()
                .unwrap()
                .iter()
                .filter(|(topic, _)| topic == AUTHORITY_UPDATES_TOPIC)
                .map(|(_, b)| rkyv::from_bytes::<AuthorityUpdate, rkyv::rancor::Error>(b).unwrap())
                .collect()
        }

        fn snapshots(&self) -> Vec<AuthoritySnapshot> {
            self.published
                .lock()
                .unwrap()
                .iter()
                .filter(|(topic, _)| topic == AUTHORITY_SNAPSHOT_REPLIES_TOPIC)
                .map(|(_, b)| {
                    rkyv::from_bytes::<AuthoritySnapshot, rkyv::rancor::Error>(b).unwrap()
                })
                .collect()
        }

        fn change(&self, section: &str, version: u64, body: Value) {
            self.publisher
                .cast(PublisherMessage::SectionChanged {
                    section: section.to_string(),
                    version,
                    body,
                    authored_by: Some("alice (serial 01ab)".to_string()),
                })
                .unwrap();
        }

        fn public_key(&self) -> rsa::RsaPublicKey {
            crate::escrow::tests::test_keypair()
                .private_key()
                .to_public_key()
        }
    }

    /// alice's account ID, which a document names her by once it is in force.
    const ALICE: &str = polar_signing::testing::ALICE_ACCOUNT_ID;

    fn policy_body(source: &str) -> Value {
        json!({"source": source})
    }

    #[tokio::test(start_paused = true)]
    async fn a_policy_that_arrives_is_signed_numbered_published_and_applied_here_too() {
        let rig = rig().await;
        rig.change(
            "policy",
            3,
            policy_body(
                r#"permit(principal == Identity::"alice", action == Action::"retrieve", resource);"#,
            ),
        );
        settle().await;

        let updates = rig.updates();
        assert_eq!(updates.len(), 1);
        let payload = verify(&updates[0], &rig.public_key()).unwrap();
        assert_eq!((payload.sequence, payload.version), (1, 3));
        assert_eq!(payload.authored_by.as_deref(), Some("alice (serial 01ab)"));

        // The ledger remembers the number.
        let sequence = ractor::call!(rig.ledger, LedgerMessage::AuthoritySequence).unwrap();
        assert_eq!(sequence, 1);

        // And this agent's own policy actor now decides by it, the same as everyone's, knowing alice by account ID.
        let decision = ractor::call!(rig.local.policy(), |reply| {
            polar_policy::PolicyMessage::CheckAuthorization {
                facts: Default::default(),
                requester_identity: ALICE.to_string(),
                requester_certificate_pem: None,
                action: r#"Action::"retrieve""#.to_string(),
                resource: r#"Secret::"s""#.to_string(),
                reply,
            }
        })
        .unwrap()
        .unwrap();
        assert_eq!(decision, polar_policy::Decision::Allow);
    }

    /// What the judge accepted is what is published: its body as it was put in force, and its effect recorded with
    /// that version's update.
    #[tokio::test(start_paused = true)]
    async fn what_the_judge_accepted_is_published_with_its_effect() {
        let rig = rig().await;
        let effect = credentials_agent_common::events::AuthorityEffect {
            gained: vec!["alice subscribe polar-authority-updates".to_string()],
            ..Default::default()
        };
        rig.acceptances.hold(
            "policy",
            4,
            crate::authority_judge::Acceptance {
                body: AuthorityBody::Policy {
                    source: "// v4, as the judge put it in force".to_string(),
                },
                effect: Some(effect.clone()),
            },
        );
        rig.change("policy", 3, policy_body("// v3"));
        rig.change("policy", 4, policy_body("// v4"));
        settle().await;
        let history = ractor::call!(rig.ledger, |reply| LedgerMessage::GetFullHistory { reply })
            .unwrap()
            .unwrap();
        let published: Vec<_> = history
            .into_iter()
            .filter_map(|(_, e)| match e {
                CredentialsEvent::AuthorityPublished {
                    version, effect, ..
                } => Some((version, effect)),
                _ => None,
            })
            .collect();
        assert_eq!(published, [(3, None), (4, Some(effect))]);
        let last = verify(&rig.updates()[1], &rig.public_key()).unwrap();
        assert_eq!(
            last.body,
            AuthorityBody::Policy {
                source: "// v4, as the judge put it in force".to_string()
            }
        );
        assert!(
            rig.acceptances.take("policy", 4).is_none(),
            "taken by the publisher"
        );
    }

    /// A section no judge saw (one held when the agent starts) is converted here the same way: a person by account ID,
    /// and one that names no account is not published.
    #[tokio::test(start_paused = true)]
    async fn a_section_no_judge_saw_is_put_in_force_by_account_id_or_not_at_all() {
        let rig = rig().await;
        rig.change(
            "policy",
            1,
            policy_body(r#"permit(principal == Identity::"carol", action, resource);"#),
        );
        settle().await;
        assert!(rig.updates().is_empty(), "carol has no account");
        rig.change(
            "groups",
            1,
            json!({"memberships": {"alice": ["operators"]}}),
        );
        settle().await;
        let payload = verify(&rig.updates()[0], &rig.public_key()).unwrap();
        match payload.body {
            AuthorityBody::Groups { memberships, .. } => {
                assert_eq!(memberships.keys().collect::<Vec<_>>(), [ALICE])
            }
            other => panic!("not groups: {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_same_or_an_older_version_is_not_published_and_spends_no_sequence() {
        let rig = rig().await;
        rig.change("policy", 2, policy_body("// v2"));
        rig.change("policy", 2, policy_body("// again"));
        rig.change("policy", 1, policy_body("// older"));
        settle().await;
        assert_eq!(rig.updates().len(), 1);
        assert_eq!(
            ractor::call!(rig.ledger, LedgerMessage::AuthoritySequence).unwrap(),
            1
        );
    }

    #[tokio::test(start_paused = true)]
    async fn groups_that_arrive_reach_the_permissions_actor_here_and_everyone() {
        let rig = rig().await;
        rig.change(
            "groups",
            1,
            json!({"memberships": {"alice": ["operators"]}}),
        );
        settle().await;
        assert_eq!(rig.updates().len(), 1);
        let principal = ractor::call!(rig.local.permissions(), |reply| {
            polar_permissions::PermissionsMessage::ResolvePrincipal {
                identity: ALICE.to_string(),
                certificate_pem: None,
                reply,
            }
        })
        .unwrap()
        .unwrap();
        assert_eq!(principal.groups, vec!["operators".to_string()]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_section_that_is_not_policy_or_groups_or_does_not_read_as_either_publishes_nothing() {
        let rig = rig().await;
        rig.change("something-else", 1, json!({}));
        rig.change("policy", 1, json!({"not_source": 1}));
        rig.change("groups", 1, json!({"memberships": "not a map"}));
        settle().await;
        assert!(rig.updates().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_revocation_is_published_as_the_whole_set_with_the_count_as_its_version() {
        let rig = rig().await;
        // Start has finished (it publishes any revocations the ledger already holds).
        settle().await;
        let revoke = |serial: &str| CredentialsEvent::Revoke {
            serial: serial.to_string(),
            reason: RevocationReason::Compromised,
            revoked_by: "admin".to_string(),
            timestamp: 1,
        };
        for serial in ["0a", "0b"] {
            ractor::call!(rig.ledger, |reply| LedgerMessage::Append {
                event: revoke(serial),
                reply
            })
            .unwrap()
            .unwrap();
        }
        rig.publisher
            .cast(PublisherMessage::RevocationsChanged)
            .unwrap();
        settle().await;
        let updates = rig.updates();
        assert_eq!(updates.len(), 1);
        let payload = verify(&updates[0], &rig.public_key()).unwrap();
        assert_eq!(payload.version, 2);
        assert_eq!(
            payload.body,
            AuthorityBody::Revoked {
                serials: vec!["0a".into(), "0b".into()]
            }
        );
        // Here too.
        use polar_agent_builder::signed::Revocation;
        let signer = polar_signing::SignerInfo {
            username: None,
            email: None,
            identity: None,
            serial: "0b".into(),
            capabilities: vec![],
        };
        assert!(rig.local.revoked().is_revoked(&signer));
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_for_the_current_authority_is_answered_once_for_everyone_in_five_seconds() {
        let rig = rig().await;
        rig.change("policy", 1, policy_body("// v1"));
        settle().await;
        let before = rig.snapshots().len();

        for _ in 0..3 {
            rig.publisher
                .cast(PublisherMessage::SnapshotRequested)
                .unwrap();
        }
        settle().await;
        let snapshots = rig.snapshots();
        assert_eq!(snapshots.len(), before + 1, "three askers, one answer");
        let last = snapshots.last().unwrap();
        assert_eq!(last.public_key().unwrap(), rig.public_key());
        assert_eq!(last.updates.len(), 1);

        tokio::time::sleep(SNAPSHOT_COALESCE + Duration::from_secs(1)).await;
        rig.publisher
            .cast(PublisherMessage::SnapshotRequested)
            .unwrap();
        settle().await;
        assert_eq!(rig.snapshots().len(), before + 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_current_authority_is_republished_after_half_an_hour_of_quiet_and_an_update_restarts_the_wait()
     {
        let rig = rig().await;
        settle().await;
        let start = rig.snapshots().len();

        // Twenty-nine minutes: nothing. Then an update, which restarts the wait.
        tokio::time::sleep(REPUBLISH_AFTER - Duration::from_secs(60)).await;
        assert_eq!(rig.snapshots().len(), start);
        rig.change("policy", 1, policy_body("// v1"));
        settle().await;
        // Two minutes on is past the original half hour, and still nothing.
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(
            rig.snapshots().len(),
            start,
            "the update restarted the wait"
        );

        // A full half hour after the update: it republishes.
        tokio::time::sleep(REPUBLISH_AFTER).await;
        assert_eq!(rig.snapshots().len(), start + 1);
        // And then waits again.
        tokio::time::sleep(REPUBLISH_AFTER).await;
        assert_eq!(rig.snapshots().len(), start + 2);
    }

    #[tokio::test(start_paused = true)]
    async fn after_a_restart_the_numbering_goes_on_where_the_ledger_left_it() {
        let rig = rig().await;
        rig.change("policy", 1, policy_body("// v1"));
        rig.change("groups", 1, json!({"memberships": {}}));
        settle().await;
        let path = rig.path.clone();
        rig.publisher.stop(None);
        rig.ledger.stop_and_wait(None, None).await.unwrap();

        let again = rig_at(&path).await;
        again.change("policy", 2, policy_body("// v2"));
        settle().await;
        let payload = verify(&again.updates()[0], &again.public_key()).unwrap();
        assert_eq!(payload.sequence, 3, "after 1 and 2, not 1 again");
    }

    #[tokio::test(start_paused = true)]
    async fn an_agent_on_the_builder_believes_everything_this_publishes() {
        let rig = rig().await;
        rig.change("policy", 1, policy_body("// v1"));
        rig.change(
            "groups",
            1,
            json!({"memberships": {"alice": ["operators"]}}),
        );
        settle().await;
        rig.publisher
            .cast(PublisherMessage::SnapshotRequested)
            .unwrap();
        settle().await;

        // A leaf that has heard nothing takes the snapshot...
        let mut leaf = AuthorityCore::new(OWN);
        let snapshot = rig.snapshots().pop().unwrap();
        let handled = leaf.on_snapshot(&snapshot, Some(OWN)).unwrap();
        assert!(handled.refused.is_empty());
        assert!(
            handled
                .effects
                .iter()
                .any(|e| matches!(e, Effect::SetPolicy(_)))
        );
        assert!(
            handled
                .effects
                .iter()
                .any(|e| matches!(e, Effect::SetGroups(..)))
        );

        // ...and a live update after it, in order, is not a missed one.
        rig.change("policy", 2, policy_body("// v2"));
        settle().await;
        let live = rig.updates().pop().unwrap();
        let handled = leaf.on_update(&live, Some(OWN)).unwrap();
        assert!(!handled.effects.contains(&Effect::AskForSnapshot));
        assert!(handled.effects.contains(&Effect::SetPolicy("// v2".into())));
    }
}
