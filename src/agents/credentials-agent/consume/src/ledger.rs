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
//! ## Graph projection: best-effort, never gating this agent's health
//!
//! Unlike every other graph-writing agent in this codebase, the
//! Credentials Agent's own liveness must never depend on Neo4j's
//! availability -- this service is too structurally critical (once
//! cert-issuer reachability narrowing lands, every other agent's own
//! cert issuance routes through it) to restart itself over a
//! dependency that isn't actually load-bearing for correctness. The
//! event log is the real source of truth and functions completely
//! independently of Neo4j; the graph is purely a disposable,
//! rebuildable observability projection. Every projection attempt
//! here is fire-and-forget: on failure, log a warning and move on --
//! never block or fail the append itself, which is what correctness
//! actually depends on.
//!
//! A `CredentialsNodeKey::ProjectionCheckpoint` singleton node tracks
//! the hash of the last event successfully incorporated into the
//! projection, using EventLog's own hash-chain position (a content-
//! derived, tamper-evident marker) rather than a separate counter.
//! This is the foundation for eventual-consistency catch-up on
//! reconnection after an outage -- the actual catch-up logic (reading
//! this checkpoint back and re-projecting only the gap) is real,
//! separate, immediately-following work, not built in this pass.

use crate::state::{CredentialsState, GrantRecord};
use credentials_agent_common::events::CredentialsEvent;
use credentials_agent_common::graph::CredentialsNodeKey;
use polar::event_log::EventLog;
use polar::graph::controller::{GraphController, GraphControllerMsg, GraphOp, GraphValue, Property, IntoGraphKey};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort, async_trait};

pub struct LedgerActor;

pub struct LedgerActorArgs {
    pub log_path: String,
    /// Spawned eagerly by the supervisor, passed in directly at
    /// construction -- not lazily discovered later, since nothing
    /// about this actor's own correctness depends on Cassini
    /// registration having happened first.
    pub graph_controller: GraphController,
}

pub struct LedgerActorState {
    event_log: EventLog<CredentialsEvent>,
    credentials_state: CredentialsState,
    graph_controller: GraphController,
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
            graph_controller: args.graph_controller,
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
                // Checked before folding -- apply() removes a
                // consumed grant from the in-memory map, so this is
                // the only point a caller can still observe whether
                // one existed prior to this specific event.
                let grant_existed = if let CredentialsEvent::IssuanceCompleted { identity, .. } =
                    &event
                {
                    state.credentials_state.grant_for(identity).is_some()
                } else {
                    false
                };

                let result = match state.event_log.append(&event) {
                    Ok(()) => {
                        state.credentials_state.apply(&event);
                        project_to_graph(state, &event, grant_existed);
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
                    // Grant/Revoke only, per deserialize_and_dispatch's
                    // own routing -- IssuanceCompleted never arrives
                    // via this path, so grant_existed is never
                    // relevant here.
                    project_to_graph(state, &event, false);
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
        }
        Ok(())
    }
}

/// Builds the graph operations for one event -- pure, and deliberately
/// separate from the actual send, so it's testable without a real
/// graph controller.
fn graph_ops_for_event(event: &CredentialsEvent, grant_existed: bool) -> Vec<GraphOp> {
    match event {
        CredentialsEvent::Grant {
            target_identity,
            permissions,
            granted_by,
            timestamp,
        } => {
            vec![GraphOp::UpsertNodes {
                items: vec![(
                    CredentialsNodeKey::Grant {
                        target_identity: target_identity.clone(),
                    }
                    .into_key(),
                    vec![
                        Property(
                            "permissions".to_string(),
                            GraphValue::List(
                                permissions
                                    .iter()
                                    .map(|p| GraphValue::String(p.clone()))
                                    .collect(),
                            ),
                        ),
                        Property(
                            "granted_by".to_string(),
                            GraphValue::String(granted_by.clone()),
                        ),
                        Property("granted_at".to_string(), GraphValue::I64(*timestamp)),
                        Property("consumed".to_string(), GraphValue::Bool(false)),
                    ],
                )],
            }]
        }
        CredentialsEvent::Revoke {
            serial,
            reason,
            revoked_by,
            timestamp,
        } => {
            vec![GraphOp::UpsertNodes {
                items: vec![(
                    CredentialsNodeKey::Certificate {
                        serial: serial.clone(),
                    }
                    .into_key(),
                    vec![
                        Property("revoked".to_string(), GraphValue::Bool(true)),
                        Property(
                            "revoked_reason".to_string(),
                            GraphValue::String(reason.clone()),
                        ),
                        Property(
                            "revoked_by".to_string(),
                            GraphValue::String(revoked_by.clone()),
                        ),
                        Property("revoked_at".to_string(), GraphValue::I64(*timestamp)),
                    ],
                )],
            }]
        }
        CredentialsEvent::IssuanceCompleted {
            identity,
            instance_binding,
            serial,
            timestamp,
        } => {
            let mut items = vec![(
                CredentialsNodeKey::Certificate {
                    serial: serial.clone(),
                }
                .into_key(),
                vec![
                    Property("identity".to_string(), GraphValue::String(identity.clone())),
                    Property(
                        "instance_binding".to_string(),
                        GraphValue::String(instance_binding.clone()),
                    ),
                    Property("issued_at".to_string(), GraphValue::I64(*timestamp)),
                    Property("revoked".to_string(), GraphValue::Bool(false)),
                ],
            )];

            // Only mark a grant consumed if one genuinely existed and
            // was actually consumed by this event -- never create a
            // spurious grant node (MERGE would otherwise happily
            // create one) for the Kubernetes-bootstrap case, where no
            // Grant event ever existed at all.
            if grant_existed {
                items.push((
                    CredentialsNodeKey::Grant {
                        target_identity: identity.clone(),
                    }
                    .into_key(),
                    vec![
                        Property("consumed".to_string(), GraphValue::Bool(true)),
                        Property("consumed_at".to_string(), GraphValue::I64(*timestamp)),
                    ],
                ));
            }

            vec![GraphOp::UpsertNodes { items }]
        }
    }
}

/// Fire-and-forget: attempts to project `event`'s effect (plus an
/// updated checkpoint) into the graph, in one atomic transaction.
/// Never fails the caller -- a failure here is logged and otherwise
/// ignored, since the event log (already durable by this point) is
/// what correctness actually depends on, not this projection.
fn project_to_graph(state: &LedgerActorState, event: &CredentialsEvent, grant_existed: bool) {
    let mut ops = graph_ops_for_event(event, grant_existed);

    ops.push(GraphOp::UpsertNodes {
        items: vec![(
            CredentialsNodeKey::ProjectionCheckpoint.into_key(),
            vec![Property(
                "last_hash".to_string(),
                GraphValue::String(state.event_log.current_hash().to_string()),
            )],
        )],
    });

    if let Err(e) = state
        .graph_controller
        .cast(GraphControllerMsg::Op(GraphOp::Transaction { ops }))
    {
        tracing::warn!(
            "Failed to project event to graph (best-effort, event log remains authoritative): {e}"
        );
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ractor::async_trait;

    struct FakeGraphController;

    #[async_trait]
    impl Actor for FakeGraphController {
        type Msg = polar::graph::controller::GraphControllerMsg;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            _message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            // Accepts and discards every op -- sufficient for tests
            // that don't care about graph behavior specifically;
            // projection is best-effort by design, so a fake that
            // simply does nothing with each op is a faithful stand-in.
            Ok(())
        }
    }

    /// Shared by every test in this crate that needs *a* GraphController
    /// but doesn't care about its actual graph behavior -- spawns fresh
    /// each call, since (unlike the escrow keypair) there's no expensive
    /// generation cost to justify caching one across tests.
    pub(crate) async fn fake_graph_controller() -> polar::graph::controller::GraphController {
        let (controller, _) = Actor::spawn(None, FakeGraphController, ())
            .await
            .expect("fake graph controller should spawn cleanly");
        controller
    }

    async fn spawn_ledger(log_path: &str) -> ActorRef<LedgerMessage> {
        let (ledger, _) = Actor::spawn(
            None,
            LedgerActor,
            LedgerActorArgs {
                log_path: log_path.to_string(),
                graph_controller: fake_graph_controller().await,
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

    #[test]
    fn graph_ops_for_grant_produces_the_correct_upsert() {
        let event = CredentialsEvent::Grant {
            target_identity: "alice".to_string(),
            permissions: vec!["read".to_string(), "write".to_string()],
            granted_by: "admin".to_string(),
            timestamp: 1000,
        };
        let ops = graph_ops_for_event(&event, false);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            GraphOp::UpsertNodes { items } => {
                assert_eq!(items.len(), 1);
            }
            other => panic!("expected UpsertNodes, got {other:?}"),
        }
    }

    #[test]
    fn graph_ops_for_issuance_only_marks_grant_consumed_when_one_existed() {
        let event = CredentialsEvent::IssuanceCompleted {
            identity: "alice".to_string(),
            instance_binding: "pod-uid-1".to_string(),
            serial: "serial-1".to_string(),
            timestamp: 2000,
        };

        let with_grant = graph_ops_for_event(&event, true);
        match &with_grant[0] {
            GraphOp::UpsertNodes { items } => assert_eq!(
                items.len(),
                2,
                "certificate node + grant-consumed node when a grant existed"
            ),
            other => panic!("expected UpsertNodes, got {other:?}"),
        }

        let without_grant = graph_ops_for_event(&event, false);
        match &without_grant[0] {
            GraphOp::UpsertNodes { items } => assert_eq!(
                items.len(),
                1,
                "only the certificate node -- no spurious grant node for the Kubernetes-bootstrap case"
            ),
            other => panic!("expected UpsertNodes, got {other:?}"),
        }
    }
}
