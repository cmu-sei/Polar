/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

//! LedgerActor: the single, serialized writer (and reader) for the
//! Credentials Agent's event log and derived state.
//!
//! The hash chain requires strict, single-writer, sequential
//! ordering -- a grant, a revocation, and an issuance all append to
//! the *same* chain, and two of those landing on independent,
//! concurrently-running actors could race on append order, corrupting
//! the tamper-evidence property the whole log exists for. Unlike
//! jira/process's topic-per-actor convention (safe there because its
//! four data streams have no ordering relationship to each other at
//! all), this needs one actor, not several -- a ractor mailbox is
//! already naturally serialized (one message processed at a time, by
//! construction), the same property that already makes
//! GraphControllerActor safe for concurrent callers to write through.
//!
//! EventLog and CredentialsState are colocated here deliberately, not
//! split across two owners -- if they were separate, a grant lookup
//! could race against a concurrent append and see a stale view. Every
//! read and write going through this same actor's mailbox gives a
//! consistent, race-free view of derived state for free.
//!
//! Graph projection is deliberately *not* owned here -- see
//! projection.rs's own module doc for why. This actor's only
//! involvement is a fire-and-forget notification to ProjectionActor
//! after each successful append+fold; it has no state of its own
//! about graph availability, checkpoints, or anything else
//! graph-related.

use crate::projection::ProjectionMessage;
use crate::state::{CredentialsState, GrantRecord};
use credentials_agent_common::events::CredentialsEvent;
use polar::event_log::EventLog;
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort, async_trait};

pub struct LedgerActor;

pub struct LedgerActorArgs {
    pub log_path: String,
    pub projection_actor: ActorRef<ProjectionMessage>,
}

pub struct LedgerActorState {
    event_log: EventLog<CredentialsEvent>,
    credentials_state: CredentialsState,
    projection_actor: ActorRef<ProjectionMessage>,
}

pub enum LedgerMessage {
    /// Appends `event` to the log, then folds it into derived state --
    /// in that order, so a reader can never observe a state change
    /// that isn't yet durable. Replies with an error string on a
    /// genuine append failure (disk I/O, or -- should it ever somehow
    /// happen at runtime rather than only at startup replay -- a
    /// broken hash chain).
    Append {
        event: CredentialsEvent,
        reply: RpcReplyPort<Result<(), String>>,
    },
    /// Fire-and-forget variant for callers with no synchronous reply
    /// possible -- grant/revocation events arriving from Cassini via
    /// deserialize_and_dispatch, a plain synchronous function that
    /// can't await a ractor::rpc::call the way Append's callers can.
    /// An append failure here is still logged loudly, just not
    /// returned to any caller.
    AppendNoReply(CredentialsEvent),
    /// The active grant for `identity`, if any.
    CheckGrant {
        identity: String,
        reply: RpcReplyPort<Option<GrantRecord>>,
    },
    /// Whether `(identity, instance_binding)` currently holds a valid,
    /// unrevoked certificate -- the renewal-prevention check itself.
    CheckRenewal {
        identity: String,
        instance_binding: String,
        reply: RpcReplyPort<bool>,
    },
    /// Full (hash, event) history, re-read from the log -- used by
    /// ProjectionActor for catch-up: replaying and re-folding through
    /// history to reconstruct correct historical context at each
    /// point (e.g. whether a grant existed at the moment a specific
    /// past event happened), not just "everything after X" taken in
    /// isolation.
    GetFullHistory {
        reply: RpcReplyPort<Result<Vec<(String, CredentialsEvent)>, String>>,
    },
}

#[async_trait]
impl Actor for LedgerActor {
    type Msg = LedgerMessage;
    type State = LedgerActorState;
    type Arguments = LedgerActorArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: LedgerActorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        let (event_log, events) = EventLog::open(&args.log_path).map_err(|e| {
            ActorProcessingErr::from(format!(
                "Failed to open event log at {}: {e}",
                args.log_path
            ))
        })?;

        let event_count = events.len();
        let credentials_state = CredentialsState::from_events(&events);
        tracing::info!(
            "LedgerActor: replayed {event_count} event(s) from {} into current state",
            args.log_path
        );

        Ok(LedgerActorState {
            event_log,
            credentials_state,
            projection_actor: args.projection_actor,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            LedgerMessage::Append { event, reply } => {
                let result = match state.event_log.append(&event) {
                    Ok(()) => {
                        state.credentials_state.apply(&event);
                        let hash = state.event_log.current_hash().to_string();
                        let _ = state.projection_actor.cast(ProjectionMessage::ProjectEvent {
                            hash,
                            event: event.clone(),
                        });
                        Ok(())
                    }
                    Err(e) => Err(e.to_string()),
                };
                let _ = reply.send(result);
            }
            LedgerMessage::AppendNoReply(event) => {
                if let Err(e) = state.event_log.append(&event) {
                    tracing::error!("Failed to append event to ledger: {e}. Event: {event:?}");
                } else {
                    state.credentials_state.apply(&event);
                    let hash = state.event_log.current_hash().to_string();
                    let _ = state.projection_actor.cast(ProjectionMessage::ProjectEvent {
                        hash,
                        event,
                    });
                }
            }
            LedgerMessage::CheckGrant { identity, reply } => {
                let grant = state.credentials_state.grant_for(&identity).cloned();
                let _ = reply.send(grant);
            }
            LedgerMessage::CheckRenewal {
                identity,
                instance_binding,
                reply,
            } => {
                let has_valid = state
                    .credentials_state
                    .has_valid_certificate(&identity, &instance_binding);
                let _ = reply.send(has_valid);
            }
            LedgerMessage::GetFullHistory { reply } => {
                let result = state.event_log.replay_with_hashes().map_err(|e| e.to_string());
                let _ = reply.send(result);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    async fn spawn_ledger(log_path: &str) -> ActorRef<LedgerMessage> {
        let (ledger, _) = Actor::spawn(
            None,
            LedgerActor,
            LedgerActorArgs {
                log_path: log_path.to_string(),
                projection_actor: crate::projection::tests::fake_projection_actor().await,
            },
        )
        .await
        .expect("ledger should spawn cleanly");
        ledger
    }

    pub(crate) async fn append(
        ledger: &ActorRef<LedgerMessage>,
        event: CredentialsEvent,
    ) -> Result<(), String> {
        match ractor::rpc::call(ledger, |reply| LedgerMessage::Append { event, reply }, None)
            .await
            .expect("call should succeed")
        {
            ractor::rpc::CallResult::Success(result) => result,
            other => panic!("expected Success, got {other:?}"),
        }
    }

    pub(crate) async fn check_grant(
        ledger: &ActorRef<LedgerMessage>,
        identity: &str,
    ) -> Option<GrantRecord> {
        match ractor::rpc::call(
            ledger,
            |reply| LedgerMessage::CheckGrant {
                identity: identity.to_string(),
                reply,
            },
            None,
        )
        .await
        .expect("call should succeed")
        {
            ractor::rpc::CallResult::Success(result) => result,
            other => panic!("expected Success, got {other:?}"),
        }
    }

    pub(crate) async fn check_renewal(
        ledger: &ActorRef<LedgerMessage>,
        identity: &str,
        instance_binding: &str,
    ) -> bool {
        match ractor::rpc::call(
            ledger,
            |reply| LedgerMessage::CheckRenewal {
                identity: identity.to_string(),
                instance_binding: instance_binding.to_string(),
                reply,
            },
            None,
        )
        .await
        .expect("call should succeed")
        {
            ractor::rpc::CallResult::Success(result) => result,
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn append_then_check_grant_round_trips_through_real_actor_messages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let ledger = spawn_ledger(path.to_str().unwrap()).await;

        assert!(check_grant(&ledger, "alice").await.is_none());

        append(
            &ledger,
            CredentialsEvent::Grant {
                target_identity: "alice".to_string(),
                permissions: vec!["read".to_string()],
                granted_by: "admin".to_string(),
                timestamp: 1000,
            },
        )
        .await
        .unwrap();

        let grant = check_grant(&ledger, "alice").await;
        assert!(grant.is_some());
        assert_eq!(grant.unwrap().granted_by, "admin");

        ledger.stop(None);
    }

    #[tokio::test]
    async fn issuance_consumes_grant_and_enables_renewal_check_through_real_actor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let ledger = spawn_ledger(path.to_str().unwrap()).await;

        append(
            &ledger,
            CredentialsEvent::Grant {
                target_identity: "alice".to_string(),
                permissions: vec!["read".to_string()],
                granted_by: "admin".to_string(),
                timestamp: 1000,
            },
        )
        .await
        .unwrap();

        append(
            &ledger,
            CredentialsEvent::IssuanceCompleted {
                identity: "alice".to_string(),
                instance_binding: "pod-uid-1".to_string(),
                serial: "serial-1".to_string(),
                timestamp: 2000,
            },
        )
        .await
        .unwrap();

        assert!(check_grant(&ledger, "alice").await.is_none());
        assert!(check_renewal(&ledger, "alice", "pod-uid-1").await);
        assert!(!check_renewal(&ledger, "alice", "different-pod-uid").await);

        ledger.stop(None);
    }

    #[tokio::test]
    async fn state_survives_a_real_stop_and_respawn_against_the_same_log() {
        // The real, end-to-end proof: not just that EventLog replays
        // correctly in isolation (already tested in lib::event_log),
        // but that stopping and respawning the actual LedgerActor
        // against the same file rebuilds correct, queryable state
        // through the real actor message-passing path.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");

        {
            let ledger = spawn_ledger(path.to_str().unwrap()).await;
            append(
                &ledger,
                CredentialsEvent::Grant {
                    target_identity: "alice".to_string(),
                    permissions: vec!["read".to_string()],
                    granted_by: "admin".to_string(),
                    timestamp: 1000,
                },
            )
            .await
            .unwrap();
            append(
                &ledger,
                CredentialsEvent::IssuanceCompleted {
                    identity: "alice".to_string(),
                    instance_binding: "pod-uid-1".to_string(),
                    serial: "serial-1".to_string(),
                    timestamp: 2000,
                },
            )
            .await
            .unwrap();
            ledger.stop_and_wait(None, None).await.unwrap();
        }

        let respawned = spawn_ledger(path.to_str().unwrap()).await;
        assert!(
            check_grant(&respawned, "alice").await.is_none(),
            "grant must still be consumed after a real restart"
        );
        assert!(
            check_renewal(&respawned, "alice", "pod-uid-1").await,
            "the issued certificate must still be on record after a real restart"
        );

        respawned.stop(None);
    }
}
