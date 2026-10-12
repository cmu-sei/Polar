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
use crate::state::{CertificateRecord, CredentialsState, GrantRecord};
use credentials_agent_common::events::CredentialsEvent;
use polar::event_log::EventLog;
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, RpcReplyPort, async_trait};
use std::sync::Arc;

pub struct LedgerActor;

pub struct LedgerActorArgs {
    pub log_path: String,
    pub projection_actor: ActorRef<ProjectionMessage>,
}

pub struct LedgerActorState {
    event_log: EventLog<CredentialsEvent>,
    credentials_state: CredentialsState,
    projection_actor: ActorRef<ProjectionMessage>,
    /// Told when an event adds a revoked serial, for whoever publishes the set to the agents.
    revocation_changes: Arc<OutputPort<RevokedChanged>>,
}

/// Why a secret was not stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretStoreError {
    /// Another identity stored a secret by this name.
    NameTaken,
    /// The ledger could not append it.
    Ledger(String),
}

/// An event added to the set of revoked certificate serials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokedChanged {
    /// How many serials are revoked now.
    pub count: usize,
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
    /// Assigns the next sequence number to an update of `kind` at `version` that is about to be published, and
    /// records that it was, in one append. The numbering is the ledger's so that it goes on rising across
    /// restarts. Replies with the sequence.
    PublishAuthority {
        kind: String,
        version: u64,
        reply: RpcReplyPort<Result<u64, String>>,
    },
    /// The highest sequence number published so far; 0 before any.
    AuthoritySequence(RpcReplyPort<u64>),
    /// The serial of every certificate an explicit revocation named, in order.
    RevokedSerials(RpcReplyPort<Vec<String>>),
    /// The port on which a newly revoked serial is announced.
    RevocationChanges(RpcReplyPort<Arc<OutputPort<RevokedChanged>>>),
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
    /// The current, non-revoked certificate for `identity`, if any --
    /// regardless of instance_binding. Backs the certificate-lookup
    /// query: one agent asking for another's public cert so it can
    /// encrypt a payload directly to it.
    CheckCertificate {
        identity: String,
        reply: RpcReplyPort<Option<CertificateRecord>>,
    },
    /// Whether `identity` already holds a currently-issued agent
    /// escrow-identity (CertType::Service) certificate -- the
    /// identity-scoped analog of CheckRenewal's own
    /// (identity, instance_binding)-scoped check for ordinary mTLS
    /// issuance. AgentEscrowIssuanceHandler's own version of "pods
    /// cannot request their own renewal." A bare bool, mirroring
    /// CheckRenewal's own shape exactly -- not CheckCertificate's,
    /// since this consumer never needs the full record, only whether
    /// one exists. CheckAgentEscrowKey, below, is the record-
    /// returning counterpart for the consumer that does.
    CheckAgentEscrowRenewal {
        identity: String,
        reply: RpcReplyPort<bool>,
    },
    /// The current agent escrow-identity (CertType::Service)
    /// certificate record for `identity`, if one has ever been
    /// issued -- mirrors CheckCertificate's own Option<Record> shape.
    /// Backs the agent escrow-key query: AgentEscrowKeyHandler
    /// genuinely needs the full record (its certificate_pem
    /// specifically, to extract the public key from) to answer that
    /// query, unlike CheckAgentEscrowRenewal above.
    CheckAgentEscrowKey {
        identity: String,
        reply: RpcReplyPort<Option<crate::state::AgentEscrowCertificateRecord>>,
    },
    /// The provisioned-but-unclaimed account record for `username`,
    /// if any -- the claim-flow's own analog of CheckGrant. `None`
    /// means either never provisioned, or already claimed (single-use,
    /// same as a Grant).
    CheckProvisionedAccount {
        username: String,
        reply: RpcReplyPort<Option<crate::state::ProvisionedAccountRecord>>,
    },
    /// Whether the one-time admin-bootstrap path has ever succeeded --
    /// checked before allowing a via_bootstrap AccountProvisioned to
    /// fold a second time.
    CheckBootstrapCompleted { reply: RpcReplyPort<bool> },
    /// The account certificate with this serial, revoked or not.
    CheckAccountCertificateBySerial {
        serial: String,
        reply: RpcReplyPort<Option<crate::state::AccountCertificateRecord>>,
    },
    /// The account holding (or last holding) this username, if any.
    CheckAccountForUsername {
        username: String,
        reply: RpcReplyPort<Option<crate::state::AccountCertificateRecord>>,
    },
    /// Records a signature-verified admin action atomically: refused
    /// (`Ok(false)`) if its payload hash was already recorded, appended
    /// and folded (`Ok(true)`) otherwise. The check and the append are
    /// one message on the single-writer actor, so two identical
    /// submissions can never both be recorded.
    RecordAdminAction {
        event: CredentialsEvent,
        reply: RpcReplyPort<Result<bool, String>>,
    },
    /// The current, escrowed secret for `name`, if one has ever been
    /// stored -- backs the general secret-retrieve mechanism.
    /// Mirrors CheckCertificate/CheckAgentEscrowKey's own
    /// Option<T> shape.
    /// Stores a secret, if its name is free or already the storing identity's own: checked and appended in
    /// one step, so two identities racing for a fresh name cannot both win.
    StoreSecret {
        event: CredentialsEvent,
        reply: RpcReplyPort<Result<(), SecretStoreError>>,
    },
    CheckSecret {
        name: String,
        reply: RpcReplyPort<Option<crate::state::StoredSecret>>,
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
            revocation_changes: Arc::new(OutputPort::default()),
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            LedgerMessage::StoreSecret { event, reply } => {
                let result = match &event {
                    CredentialsEvent::SecretStored {
                        name, stored_by, ..
                    } => {
                        if state.credentials_state.may_store_secret(name, stored_by) {
                            append_and_project(state, &event).map_err(SecretStoreError::Ledger)
                        } else {
                            Err(SecretStoreError::NameTaken)
                        }
                    }
                    _ => Err(SecretStoreError::Ledger(
                        "not a SecretStored event".to_string(),
                    )),
                };
                let _ = reply.send(result);
            }
            LedgerMessage::Append { event, reply } => {
                let result = append_and_project(state, &event);
                let _ = reply.send(result);
            }
            LedgerMessage::PublishAuthority {
                kind,
                version,
                reply,
            } => {
                let sequence = state.credentials_state.last_authority_sequence() + 1;
                let result = append_and_project(
                    state,
                    &CredentialsEvent::AuthorityPublished {
                        sequence,
                        kind,
                        version,
                        timestamp: polar::time::now_millis(),
                    },
                )
                .map(|()| sequence);
                let _ = reply.send(result);
            }
            LedgerMessage::AuthoritySequence(reply) => {
                let _ = reply.send(state.credentials_state.last_authority_sequence());
            }
            LedgerMessage::RevokedSerials(reply) => {
                let _ = reply.send(state.credentials_state.revoked_serials());
            }
            LedgerMessage::RevocationChanges(reply) => {
                let _ = reply.send(state.revocation_changes.clone());
            }
            LedgerMessage::AppendNoReply(event) => {
                if let Err(e) = append_and_project(state, &event) {
                    tracing::error!("Failed to append event to ledger: {e}. Event: {event:?}");
                }
            }
            LedgerMessage::RecordAdminAction { event, reply } => {
                let CredentialsEvent::AdminActionRecorded { payload_hash, .. } = &event else {
                    let _ = reply.send(Err("not an AdminActionRecorded event".to_string()));
                    return Ok(());
                };
                if state.credentials_state.admin_action_seen(payload_hash) {
                    let _ = reply.send(Ok(false));
                    return Ok(());
                }
                let result = append_and_project(state, &event).map(|()| true);
                let _ = reply.send(result);
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
            LedgerMessage::CheckCertificate { identity, reply } => {
                let record = state.credentials_state.certificate_for(&identity).cloned();
                let _ = reply.send(record);
            }
            LedgerMessage::CheckAgentEscrowRenewal { identity, reply } => {
                let has_one = state
                    .credentials_state
                    .has_agent_escrow_certificate(&identity);
                let _ = reply.send(has_one);
            }
            LedgerMessage::CheckAgentEscrowKey { identity, reply } => {
                let record = state
                    .credentials_state
                    .agent_escrow_certificate_for(&identity)
                    .cloned();
                let _ = reply.send(record);
            }
            LedgerMessage::CheckProvisionedAccount { username, reply } => {
                let record = state
                    .credentials_state
                    .provisioned_account_for(&username)
                    .cloned();
                let _ = reply.send(record);
            }
            LedgerMessage::CheckBootstrapCompleted { reply } => {
                let _ = reply.send(state.credentials_state.bootstrap_completed());
            }
            LedgerMessage::CheckAccountCertificateBySerial { serial, reply } => {
                let record = state
                    .credentials_state
                    .account_certificate_by_serial(&serial)
                    .cloned();
                let _ = reply.send(record);
            }
            LedgerMessage::CheckAccountForUsername { username, reply } => {
                let record = state
                    .credentials_state
                    .account_for_username(&username)
                    .cloned();
                let _ = reply.send(record);
            }
            LedgerMessage::CheckSecret { name, reply } => {
                let secret = state.credentials_state.secret_for(&name).cloned();
                let _ = reply.send(secret);
            }
            LedgerMessage::GetFullHistory { reply } => {
                let result = state
                    .event_log
                    .replay_with_hashes()
                    .map_err(|e| e.to_string());
                let _ = reply.send(result);
            }
        }
        Ok(())
    }
}

/// Appends `event` durably, folds it into derived state, and notifies
/// the projection -- in that order, so a reader can never observe a
/// state change that isn't yet durable.
fn append_and_project(
    state: &mut LedgerActorState,
    event: &CredentialsEvent,
) -> Result<(), String> {
    state.event_log.append(event).map_err(|e| e.to_string())?;
    let revoked_before = state.credentials_state.revoked_serials().len();
    let superseded_serials = state.credentials_state.apply(event);
    let revoked_after = state.credentials_state.revoked_serials().len();
    if revoked_after > revoked_before {
        state.revocation_changes.send(RevokedChanged {
            count: revoked_after,
        });
    }
    let hash = state.event_log.current_hash().to_string();
    let _ = state
        .projection_actor
        .cast(ProjectionMessage::ProjectEvent {
            hash,
            event: event.clone(),
            superseded_serials,
        });
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use credentials_agent_common::events::GrantReason;

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

    pub(crate) async fn check_certificate(
        ledger: &ActorRef<LedgerMessage>,
        identity: &str,
    ) -> Option<CertificateRecord> {
        match ractor::rpc::call(
            ledger,
            |reply| LedgerMessage::CheckCertificate {
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

    pub(crate) async fn check_agent_escrow_renewal(
        ledger: &ActorRef<LedgerMessage>,
        identity: &str,
    ) -> bool {
        match ractor::rpc::call(
            ledger,
            |reply| LedgerMessage::CheckAgentEscrowRenewal {
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

    pub(crate) async fn check_agent_escrow_key(
        ledger: &ActorRef<LedgerMessage>,
        identity: &str,
    ) -> Option<crate::state::AgentEscrowCertificateRecord> {
        match ractor::rpc::call(
            ledger,
            |reply| LedgerMessage::CheckAgentEscrowKey {
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

    async fn publish_authority(
        ledger: &ActorRef<LedgerMessage>,
        kind: &str,
        version: u64,
    ) -> Result<u64, String> {
        match ractor::rpc::call(
            ledger,
            |reply| LedgerMessage::PublishAuthority {
                kind: kind.to_string(),
                version,
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

    async fn last_authority_sequence(ledger: &ActorRef<LedgerMessage>) -> u64 {
        match ractor::rpc::call(ledger, LedgerMessage::AuthoritySequence, None)
            .await
            .expect("call should succeed")
        {
            ractor::rpc::CallResult::Success(result) => result,
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn authority_sequences_are_assigned_in_order_and_go_on_rising_across_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let ledger = spawn_ledger(path.to_str().unwrap()).await;
        assert_eq!(last_authority_sequence(&ledger).await, 0);
        assert_eq!(publish_authority(&ledger, "policy", 1).await, Ok(1));
        assert_eq!(publish_authority(&ledger, "groups", 1).await, Ok(2));
        assert_eq!(last_authority_sequence(&ledger).await, 2);
        ledger.stop_and_wait(None, None).await.unwrap();

        // The agent restarts: the ledger remembers where the numbering was.
        let ledger = spawn_ledger(path.to_str().unwrap()).await;
        assert_eq!(last_authority_sequence(&ledger).await, 2);
        assert_eq!(publish_authority(&ledger, "policy", 2).await, Ok(3));
        ledger.stop(None);
    }

    #[tokio::test]
    async fn the_revoked_serials_are_asked_of_the_ledger_and_a_change_is_announced() {
        use ractor::OutputPort;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let ledger = spawn_ledger(path.to_str().unwrap()).await;

        let port: std::sync::Arc<OutputPort<RevokedChanged>> =
            match ractor::rpc::call(&ledger, LedgerMessage::RevocationChanges, None)
                .await
                .unwrap()
            {
                ractor::rpc::CallResult::Success(port) => port,
                other => panic!("{other:?}"),
            };
        struct Sink(std::sync::Arc<std::sync::Mutex<Vec<RevokedChanged>>>);
        #[async_trait]
        impl Actor for Sink {
            type Msg = RevokedChanged;
            type State = ();
            type Arguments = ();
            async fn pre_start(
                &self,
                _: ActorRef<Self::Msg>,
                _: (),
            ) -> Result<(), ActorProcessingErr> {
                Ok(())
            }
            async fn handle(
                &self,
                _: ActorRef<Self::Msg>,
                message: Self::Msg,
                _: &mut (),
            ) -> Result<(), ActorProcessingErr> {
                self.0.lock().unwrap().push(message);
                Ok(())
            }
        }
        let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (sink, _) = Actor::spawn(None, Sink(heard.clone()), ()).await.unwrap();
        port.subscribe(sink, Some);

        let revoke = |serial: &str| CredentialsEvent::Revoke {
            serial: serial.to_string(),
            reason: credentials_agent_common::events::RevocationReason::Compromised,
            revoked_by: "admin".to_string(),
            timestamp: 1000,
        };
        append(&ledger, revoke("0a")).await.unwrap();
        append(&ledger, revoke("0a")).await.unwrap(); // again: nothing new
        append(&ledger, revoke("0b")).await.unwrap();
        // An event that changes nothing about revocation announces nothing.
        append(
            &ledger,
            CredentialsEvent::AuthorityPublished {
                sequence: 1,
                kind: "policy".into(),
                version: 1,
                timestamp: 1,
            },
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        let serials = match ractor::rpc::call(&ledger, LedgerMessage::RevokedSerials, None)
            .await
            .unwrap()
        {
            ractor::rpc::CallResult::Success(serials) => serials,
            other => panic!("{other:?}"),
        };
        assert_eq!(serials, ["0a", "0b"]);
        assert_eq!(
            heard.lock().unwrap().len(),
            2,
            "one announcement per new serial"
        );
        ledger.stop(None);
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
                permissions: vec![cert_issuer_common::permission::Permission {
                    scope: cert_issuer_common::permission::PermissionScope::ServiceToService,
                    target: None,
                }],
                granted_by: "admin".to_string(),
                timestamp: 1000,
                reason: GrantReason::UserOnboarding,
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
                permissions: vec![cert_issuer_common::permission::Permission {
                    scope: cert_issuer_common::permission::PermissionScope::ServiceToService,
                    target: None,
                }],
                granted_by: "admin".to_string(),
                timestamp: 1000,
                reason: GrantReason::UserOnboarding,
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
                certificate_pem: "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
                    .to_string(),
            },
        )
        .await
        .unwrap();

        assert!(check_grant(&ledger, "alice").await.is_none());
        assert!(check_renewal(&ledger, "alice", "pod-uid-1").await);
        assert!(!check_renewal(&ledger, "alice", "different-pod-uid").await);

        let cert = check_certificate(&ledger, "alice").await;
        assert!(cert.is_some());
        assert_eq!(
            cert.unwrap().certificate_pem,
            "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
        );
        assert!(check_certificate(&ledger, "nobody").await.is_none());

        ledger.stop(None);
    }

    #[tokio::test]
    async fn agent_escrow_issuance_is_visible_through_the_real_actor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let ledger = spawn_ledger(path.to_str().unwrap()).await;

        assert!(!check_agent_escrow_renewal(&ledger, "git-observer").await);
        assert!(
            check_agent_escrow_key(&ledger, "git-observer")
                .await
                .is_none()
        );

        append(
            &ledger,
            CredentialsEvent::AgentEscrowIssuanceCompleted {
                identity: "git-observer".to_string(),
                instance_binding: "pod-uid-1".to_string(),
                serial: "service-serial-1".to_string(),
                timestamp: 2000,
                certificate_pem:
                    "-----BEGIN CERTIFICATE-----\nfake-service-cert\n-----END CERTIFICATE-----\n"
                        .to_string(),
            },
        )
        .await
        .unwrap();

        assert!(check_agent_escrow_renewal(&ledger, "git-observer").await);
        assert!(!check_agent_escrow_renewal(&ledger, "nobody").await);

        let record = check_agent_escrow_key(&ledger, "git-observer").await;
        assert!(record.is_some());
        assert_eq!(record.unwrap().serial, "service-serial-1");
        assert!(check_agent_escrow_key(&ledger, "nobody").await.is_none());

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
                    permissions: vec![cert_issuer_common::permission::Permission {
                        scope: cert_issuer_common::permission::PermissionScope::ServiceToService,
                        target: None,
                    }],
                    granted_by: "admin".to_string(),
                    timestamp: 1000,
                    reason: GrantReason::UserOnboarding,
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
                    certificate_pem: "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
                        .to_string(),
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

    pub(crate) async fn record_admin_action(
        ledger: &ActorRef<LedgerMessage>,
        event: CredentialsEvent,
    ) -> Result<bool, String> {
        match ractor::rpc::call(
            ledger,
            |reply| LedgerMessage::RecordAdminAction { event, reply },
            None,
        )
        .await
        .expect("call should succeed")
        {
            ractor::rpc::CallResult::Success(result) => result,
            other => panic!("expected Success, got {other:?}"),
        }
    }

    fn recorded_grant(nonce: &str) -> CredentialsEvent {
        use credentials_agent_common::admin_action::{
            AdminAction, AdminActionBody, SignedAdminAction,
        };
        let payload = AdminActionBody {
            nonce: nonce.to_string(),
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
        CredentialsEvent::AdminActionRecorded {
            payload_hash: signed.payload_hash(),
            signed,
            author_account_id: "admin-acct".to_string(),
            timestamp: 5000,
            account_id: None,
        }
    }

    #[tokio::test]
    async fn a_recorded_admin_action_takes_effect_once_and_a_replay_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let ledger = spawn_ledger(path.to_str().unwrap()).await;

        assert_eq!(
            record_admin_action(&ledger, recorded_grant("n1")).await,
            Ok(true)
        );
        assert_eq!(
            check_grant(&ledger, "alice").await.unwrap().granted_by,
            "admin-acct"
        );

        // The identical signed payload is a replay.
        assert_eq!(
            record_admin_action(&ledger, recorded_grant("n1")).await,
            Ok(false)
        );
        // The same action with a fresh nonce is a legitimate repeat.
        assert_eq!(
            record_admin_action(&ledger, recorded_grant("n2")).await,
            Ok(true)
        );

        ledger.stop(None);
    }
}
