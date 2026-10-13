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

//! ProjectionActor: owns the Neo4j graph projection as its own,
//! genuinely separate concern from the event log -- the ledger is
//! not concerned with the graph, other than as a facilitator, so it
//! isn't the right ownership layer for this despite being where the
//! events (and the graph ops derived from them) originate. This actor
//! owns GraphControllerActor as its own direct child (spawned here,
//! not by the supervisor), handling its own lifecycle correctly --
//! shutdown from the supervisor propagates down through this actor to
//! its own graph controller child naturally, rather than needing to
//! traverse an unrelated actor (LedgerActor) that has no real stake
//! in the graph's own lifecycle.
//!
//! Two paths populate the graph:
//!   - The steady-state case: LedgerActor sends ProjectEvent for each
//!     event, fire-and-forget, immediately after a successful
//!     append+fold -- the same best-effort nature already established.
//!   - Catch-up: on GraphSignal::Available (the graph controller
//!     reconnecting, whether at startup or after an outage), this
//!     actor reads the ProjectionCheckpoint node back, asks
//!     LedgerActor for the full event history, replays and re-folds
//!     it sequentially (so historical context like "did a grant exist
//!     at this exact point" is reconstructed correctly, not
//!     approximated), and emits graph ops only for whatever happened
//!     after the checkpoint.

use crate::ledger::LedgerMessage;
use crate::state::CredentialsState;
use credentials_agent_common::events::{CredentialsEvent, RevocationReason};
use credentials_agent_common::graph::CredentialsNodeKey;
use polar::graph::controller::{
    GraphController, GraphControllerActor, GraphControllerArgs, GraphControllerMsg, GraphOp,
    GraphSignal, GraphValue, IntoGraphKey, Property,
};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, async_trait};
use std::sync::Arc;
use tracing::{error, warn};

pub struct ProjectionActor;

pub struct ProjectionActorArgs {
    /// Deferred rather than required at construction -- LedgerActor
    /// needs a reference to *this* actor to notify it after every
    /// append, and this actor needs a reference back to LedgerActor
    /// for grant-existed checks and catch-up, so neither can hold the
    /// other's ActorRef before both are already spawned. Set shortly
    /// after startup via SetLedger, once LedgerActor actually exists.
    pub ledger: Option<ActorRef<LedgerMessage>>,
    /// How to reach the graph, from the builder's snapshot of the environment.
    pub graph: polar::graph::environment::GraphSettings,
}

pub struct ProjectionActorState {
    ledger: Option<ActorRef<LedgerMessage>>,
    graph_controller: GraphController,
}

pub enum ProjectionMessage {
    /// Fire-and-forget: project one event's effect, best-effort --
    /// the steady-state path, sent by LedgerActor after each
    /// successful append+fold. `hash` is that event's own resulting
    /// hash (EventLog::current_hash() at the moment it was appended),
    /// carried along so this actor can keep the checkpoint accurate
    /// without needing to ask the ledger for it separately.
    ProjectEvent {
        hash: String,
        event: CredentialsEvent,
        /// Serial numbers of any certificates superseded as a side
        /// effect of folding this event -- CredentialsState::apply's
        /// own return value, threaded through so the graph's copy of
        /// those certificates gets marked revoked too, not just the
        /// in-memory one.
        superseded_serials: Vec<String>,
    },
    GraphSignal(GraphSignal),
    /// Sent once, shortly after startup, once LedgerActor exists --
    /// see ProjectionActorArgs::ledger's own doc for why this can't
    /// just be a constructor argument.
    SetLedger(ActorRef<LedgerMessage>),
}

#[async_trait]
impl Actor for ProjectionActor {
    type Msg = ProjectionMessage;
    type State = ProjectionActorState;
    type Arguments = ProjectionActorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ProjectionActorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        let signal_port = Arc::new(OutputPort::<GraphSignal>::default());
        signal_port.subscribe(myself.clone(), |signal| {
            Some(ProjectionMessage::GraphSignal(signal))
        });

        let (graph_controller, _) = GraphControllerActor::spawn_linked(
            Some("credentials-agent.projection.graph.controller".to_string()),
            GraphControllerActor,
            GraphControllerArgs::new(signal_port).with_settings(args.graph.clone()),
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        Ok(ProjectionActorState {
            ledger: args.ledger,
            graph_controller,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ProjectionMessage::ProjectEvent {
                hash,
                event,
                superseded_serials,
            } => {
                let Some(ledger) = &state.ledger else {
                    warn!("Dropping event projection -- ledger reference not yet set");
                    return Ok(());
                };
                project_single_event(
                    &state.graph_controller,
                    &hash,
                    &event,
                    &superseded_serials,
                    ledger,
                )
                .await;
            }
            ProjectionMessage::GraphSignal(GraphSignal::Available) => {
                let Some(ledger) = &state.ledger else {
                    warn!("Skipping catch-up -- ledger reference not yet set");
                    return Ok(());
                };
                catch_up(ledger, &state.graph_controller).await;
            }
            ProjectionMessage::GraphSignal(GraphSignal::OpFailed(reason)) => {
                warn!(
                    "Graph projection op failed (best-effort, event log remains authoritative): {reason}"
                );
            }
            ProjectionMessage::SetLedger(ledger) => {
                state.ledger = Some(ledger);
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
            reason,
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
                                    .map(|p| GraphValue::String(p.to_san_string()))
                                    .collect(),
                            ),
                        ),
                        Property(
                            "granted_by".to_string(),
                            GraphValue::String(granted_by.clone()),
                        ),
                        Property("granted_at".to_string(), GraphValue::I64(*timestamp)),
                        Property("consumed".to_string(), GraphValue::Bool(false)),
                        Property(
                            "reason".to_string(),
                            GraphValue::String(format!("{reason:?}")),
                        ),
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
                            GraphValue::String(format!("{reason:?}")),
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
            // A client or a server certificate is a Certificate node alike; the graph keeps no more
            // about it than it does about either's PEM.
            cert_type: _,
            // Deliberately not projected -- the graph stays
            // metadata-only, observability-only; a full cert PEM
            // doesn't belong there. Lookup goes through LedgerActor's
            // own CredentialsState directly, not the graph.
            certificate_pem: _,
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
        // Deliberately not projected -- the graph stays
        // metadata-only, observability-only; secret material,
        // encrypted or not, doesn't belong there any more than a
        // full cert PEM does. Lookup goes through LedgerActor's own
        // CredentialsState directly, not the graph.
        CredentialsEvent::SecretStored { .. } => vec![],
        // That an update was published is recorded in the ledger for its sequence number, and the graph has
        // nothing to say about it.
        CredentialsEvent::AuthorityPublished { .. } => vec![],
        // An admin action projects exactly what its derived effect
        // does: the same single derivation state folds.
        CredentialsEvent::AdminActionRecorded { .. } => match event.admin_effect() {
            Some(effect) => graph_ops_for_event(&effect, grant_existed),
            None => vec![],
        },
        CredentialsEvent::AgentEscrowIssuanceCompleted {
            identity,
            instance_binding,
            serial,
            timestamp,
            // Deliberately not projected, same reasoning as
            // IssuanceCompleted's own certificate_pem above.
            certificate_pem: _,
        } => {
            let mut items = vec![(
                CredentialsNodeKey::AgentEscrowCertificate {
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
                ],
            )];

            // Same reasoning as IssuanceCompleted's own grant-existed
            // check above -- never create a spurious grant node for
            // the (far more common, given the uniform-permission
            // default) case where no Grant event ever existed for
            // this identity at all.
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
        CredentialsEvent::AccountProvisioned {
            account_id,
            username,
            email,
            provisioned_by,
            timestamp,
            reason,
            via_bootstrap,
            // Permissions, capabilities and the CSR-overridable
            // personal-detail defaults deliberately not projected -- same
            // metadata-only, observability-only discipline as every
            // other event here. An operator wanting the full record
            // reads it from the ledger's own derived state, not the
            // graph.
            permissions: _,
            capabilities: _,
            given_name: _,
            surname: _,
            title: _,
            telephone_number: _,
        } => {
            vec![GraphOp::UpsertNodes {
                items: vec![(
                    CredentialsNodeKey::Account {
                        account_id: account_id.clone(),
                    }
                    .into_key(),
                    vec![
                        Property("username".to_string(), GraphValue::String(username.clone())),
                        Property("email".to_string(), GraphValue::String(email.clone())),
                        Property(
                            "provisioned_by".to_string(),
                            GraphValue::String(provisioned_by.clone()),
                        ),
                        Property("provisioned_at".to_string(), GraphValue::I64(*timestamp)),
                        Property(
                            "reason".to_string(),
                            GraphValue::String(format!("{reason:?}")),
                        ),
                        Property(
                            "via_bootstrap".to_string(),
                            GraphValue::Bool(*via_bootstrap),
                        ),
                        Property("claimed".to_string(), GraphValue::Bool(false)),
                    ],
                )],
            }]
        }
        CredentialsEvent::AccountCertificateIssued {
            account_id,
            username,
            serial,
            timestamp,
            // Deliberately not projected, same reasoning as
            // IssuanceCompleted's own certificate_pem above.
            certificate_pem: _,
        } => {
            vec![GraphOp::UpsertNodes {
                items: vec![
                    (
                        CredentialsNodeKey::Account {
                            account_id: account_id.clone(),
                        }
                        .into_key(),
                        vec![
                            Property("claimed".to_string(), GraphValue::Bool(true)),
                            Property("claimed_at".to_string(), GraphValue::I64(*timestamp)),
                            Property(
                                "current_serial".to_string(),
                                GraphValue::String(serial.clone()),
                            ),
                        ],
                    ),
                    (
                        CredentialsNodeKey::Certificate {
                            serial: serial.clone(),
                        }
                        .into_key(),
                        vec![
                            Property(
                                "identity".to_string(),
                                GraphValue::String(account_id.clone()),
                            ),
                            Property("username".to_string(), GraphValue::String(username.clone())),
                            Property("issued_at".to_string(), GraphValue::I64(*timestamp)),
                            Property("revoked".to_string(), GraphValue::Bool(false)),
                        ],
                    ),
                ],
            }]
        }
    }
}

fn checkpoint_op(last_hash: &str) -> GraphOp {
    GraphOp::UpsertNodes {
        items: vec![(
            CredentialsNodeKey::ProjectionCheckpoint.into_key(),
            vec![Property(
                "last_hash".to_string(),
                GraphValue::String(last_hash.to_string()),
            )],
        )],
    }
}

/// Fire-and-forget: attempts to project `event`'s effect (plus an
/// updated checkpoint) into the graph, in one atomic transaction.
/// Never fails the caller -- a failure here is logged and otherwise
/// ignored, since the event log (already durable well before this
/// point) is what correctness actually depends on, not this
/// projection. grant_existed is looked up fresh from the ledger here
/// (a real, small extra round trip) rather than threaded through from
/// the caller, since ProjectionActor -- correctly -- has no state of
/// its own to check it against.
async fn project_single_event(
    graph_controller: &GraphController,
    hash: &str,
    event: &CredentialsEvent,
    superseded_serials: &[String],
    ledger: &ActorRef<LedgerMessage>,
) {
    // Only a client certificate uses up a grant (state.rs), so only it can have found one.
    let grant_existed = if let CredentialsEvent::IssuanceCompleted {
        identity,
        cert_type,
        ..
    } = event
        && cert_type.is_client()
    {
        matches!(
            ractor::rpc::call(
                ledger,
                |reply| LedgerMessage::CheckGrant {
                    identity: identity.clone(),
                    reply,
                },
                None,
            )
            .await,
            Ok(ractor::rpc::CallResult::Success(Some(_)))
        )
    } else {
        false
    };

    let mut ops = graph_ops_for_event(event, grant_existed);

    // A reissue implicitly supersedes any prior, still-valid
    // certificate for the same identity -- CredentialsState::apply's
    // own return value names exactly which ones. Each gets its own
    // revocation op here, synthetic (no real Revoke event exists for
    // this), so the graph's copy doesn't silently drift out of sync
    // with the in-memory state that already knows this happened.
    let timestamp = event_timestamp(event);
    for serial in superseded_serials {
        ops.push(supersession_op(serial, timestamp));
    }

    ops.push(checkpoint_op(hash));

    if let Err(e) = graph_controller.cast(GraphControllerMsg::Op(GraphOp::Transaction { ops })) {
        warn!(
            "Failed to project event to graph (best-effort, event log remains authoritative): {e}"
        );
    }
}

fn event_timestamp(event: &CredentialsEvent) -> i64 {
    match event {
        CredentialsEvent::Grant { timestamp, .. }
        | CredentialsEvent::Revoke { timestamp, .. }
        | CredentialsEvent::IssuanceCompleted { timestamp, .. }
        | CredentialsEvent::SecretStored { timestamp, .. }
        | CredentialsEvent::AgentEscrowIssuanceCompleted { timestamp, .. }
        | CredentialsEvent::AccountProvisioned { timestamp, .. }
        | CredentialsEvent::AccountCertificateIssued { timestamp, .. }
        | CredentialsEvent::AuthorityPublished { timestamp, .. }
        | CredentialsEvent::AdminActionRecorded { timestamp, .. } => *timestamp,
    }
}

fn supersession_op(serial: &str, timestamp: i64) -> GraphOp {
    GraphOp::UpsertNodes {
        items: vec![(
            CredentialsNodeKey::Certificate {
                serial: serial.to_string(),
            }
            .into_key(),
            vec![
                Property("revoked".to_string(), GraphValue::Bool(true)),
                Property(
                    "revoked_reason".to_string(),
                    GraphValue::String(format!("{:?}", RevocationReason::Superseded)),
                ),
                Property(
                    "revoked_by".to_string(),
                    GraphValue::String("system".to_string()),
                ),
                Property("revoked_at".to_string(), GraphValue::I64(timestamp)),
            ],
        )],
    }
}

/// Pure: given the full (hash, event) history and the last
/// successfully-projected checkpoint hash (`None` if nothing has ever
/// been projected), returns the graph ops that still need to run,
/// plus the new checkpoint value to record. Replays and folds
/// sequentially through *all* of history so historical context (like
/// whether a grant existed at the moment a specific past event
/// happened) is reconstructed correctly, but only emits ops for
/// events after the checkpoint -- everything before it already
/// succeeded, so there's nothing to redo there.
fn ops_for_catch_up(
    history: &[(String, CredentialsEvent)],
    checkpoint_hash: Option<&str>,
) -> (Vec<GraphOp>, Option<String>) {
    let mut state = CredentialsState::new();
    let mut past_checkpoint = checkpoint_hash.is_none();
    let mut ops = Vec::new();
    let mut latest_hash = None;

    for (hash, event) in history {
        if !past_checkpoint {
            state.apply(event);
            if Some(hash.as_str()) == checkpoint_hash {
                past_checkpoint = true;
            }
            continue;
        }

        let grant_existed = if let CredentialsEvent::IssuanceCompleted {
            identity,
            cert_type,
            ..
        } = event
            && cert_type.is_client()
        {
            state.grant_for(identity).is_some()
        } else {
            false
        };

        let superseded_serials = state.apply(event);
        ops.extend(graph_ops_for_event(event, grant_existed));
        for serial in &superseded_serials {
            ops.push(supersession_op(serial, event_timestamp(event)));
        }
        latest_hash = Some(hash.clone());
    }

    (ops, latest_hash)
}

async fn read_checkpoint(graph_controller: &GraphController) -> Result<Option<String>, String> {
    let query = neo4rs::Query::new(
        "MATCH (n:CredentialsProjectionCheckpoint) RETURN n.last_hash AS last_hash".to_string(),
    );

    let rows = match ractor::rpc::call(
        graph_controller,
        |reply| GraphControllerMsg::Query(query, reply),
        None,
    )
    .await
    {
        Ok(ractor::rpc::CallResult::Success(Ok(rows))) => rows,
        Ok(ractor::rpc::CallResult::Success(Err(e))) => return Err(e),
        other => {
            return Err(format!(
                "unexpected CallResult reading checkpoint: {other:?}"
            ));
        }
    };

    match rows.first() {
        Some(row) => row
            .get::<String>("last_hash")
            .map(Some)
            .map_err(|e| e.to_string()),
        None => Ok(None),
    }
}

async fn get_full_history(
    ledger: &ActorRef<LedgerMessage>,
) -> Result<Vec<(String, CredentialsEvent)>, String> {
    match ractor::rpc::call(
        ledger,
        |reply| LedgerMessage::GetFullHistory { reply },
        None,
    )
    .await
    {
        Ok(ractor::rpc::CallResult::Success(result)) => result,
        other => Err(format!("unexpected CallResult reading history: {other:?}")),
    }
}

async fn catch_up(ledger: &ActorRef<LedgerMessage>, graph_controller: &GraphController) {
    let checkpoint_hash = match read_checkpoint(graph_controller).await {
        Ok(hash) => hash,
        Err(e) => {
            warn!("Failed to read projection checkpoint, skipping catch-up: {e}");
            return;
        }
    };

    let history = match get_full_history(ledger).await {
        Ok(h) => h,
        Err(e) => {
            warn!("Failed to get event history for catch-up: {e}");
            return;
        }
    };

    let (mut ops, latest_hash) = ops_for_catch_up(&history, checkpoint_hash.as_deref());

    let Some(latest_hash) = latest_hash else {
        // Nothing new since the checkpoint -- already fully caught up.
        return;
    };

    ops.push(checkpoint_op(&latest_hash));

    if let Err(e) = graph_controller.cast(GraphControllerMsg::Op(GraphOp::Transaction { ops })) {
        error!("Failed to run catch-up projection: {e}");
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use credentials_agent_common::events::GrantReason;

    struct FakeProjectionActor;

    #[async_trait]
    impl Actor for FakeProjectionActor {
        type Msg = ProjectionMessage;
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
            // Accepts and discards every message -- sufficient for
            // tests exercising LedgerActor's own logic, which don't
            // care about actual graph projection behavior.
            Ok(())
        }
    }

    pub(crate) async fn fake_projection_actor() -> ActorRef<ProjectionMessage> {
        let (actor, _) = Actor::spawn(None, FakeProjectionActor, ())
            .await
            .expect("fake projection actor should spawn cleanly");
        actor
    }

    #[test]
    fn graph_ops_for_grant_produces_the_correct_upsert() {
        let event = CredentialsEvent::Grant {
            target_identity: "alice".to_string(),
            permissions: vec![
                cert_issuer_common::permission::Permission {
                    scope: cert_issuer_common::permission::PermissionScope::ServiceToService,
                    target: None,
                },
                cert_issuer_common::permission::Permission {
                    scope: cert_issuer_common::permission::PermissionScope::UserToService,
                    target: None,
                },
            ],
            granted_by: "admin".to_string(),
            timestamp: 1000,
            reason: GrantReason::UserOnboarding,
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
            cert_type: credentials_agent_common::issuance::WorkloadCertType::Client,
            certificate_pem: "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
                .to_string(),
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

    #[test]
    fn graph_ops_for_agent_escrow_issuance_only_marks_grant_consumed_when_one_existed() {
        let event = CredentialsEvent::AgentEscrowIssuanceCompleted {
            identity: "git-observer".to_string(),
            instance_binding: "pod-uid-1".to_string(),
            serial: "service-serial-1".to_string(),
            timestamp: 2000,
            certificate_pem:
                "-----BEGIN CERTIFICATE-----\nfake-service-cert\n-----END CERTIFICATE-----\n"
                    .to_string(),
        };
        let with_grant = graph_ops_for_event(&event, true);
        match &with_grant[0] {
            GraphOp::UpsertNodes { items } => assert_eq!(
                items.len(),
                2,
                "agent escrow certificate node + grant-consumed node when a grant existed"
            ),
            other => panic!("expected UpsertNodes, got {other:?}"),
        }
        let without_grant = graph_ops_for_event(&event, false);
        match &without_grant[0] {
            GraphOp::UpsertNodes { items } => assert_eq!(
                items.len(),
                1,
                "only the agent escrow certificate node -- no spurious grant node for the uniform-permission-default case"
            ),
            other => panic!("expected UpsertNodes, got {other:?}"),
        }
    }

    #[test]
    fn graph_ops_for_a_recorded_grant_match_the_derived_grant() {
        use credentials_agent_common::admin_action::{
            AdminAction, AdminActionBody, SignedAdminAction,
        };
        let payload = AdminActionBody {
            nonce: "n1".to_string(),
            action: AdminAction::Grant {
                target_identity: "alice".to_string(),
                permissions: vec![],
                reason: GrantReason::UserOnboarding,
            },
        }
        .to_payload()
        .unwrap();
        let signed = SignedAdminAction {
            payload,
            author_serial: "a1".to_string(),
            signature: vec![],
        };
        let recorded = CredentialsEvent::AdminActionRecorded {
            payload_hash: signed.payload_hash(),
            signed,
            author_account_id: "admin-acct".to_string(),
            timestamp: 5000,
            account_id: None,
        };
        let derived = recorded.admin_effect().unwrap();

        let from_recorded = graph_ops_for_event(&recorded, false);
        let from_derived = graph_ops_for_event(&derived, false);
        assert!(!from_recorded.is_empty());
        assert_eq!(from_recorded.len(), from_derived.len());
    }
}
