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
use credentials_agent_common::issuance::WorkloadCertType;
use polar::event_log::EventLog;
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, RpcReplyPort, async_trait};
use std::sync::Arc;
use std::time::Duration;

pub struct LedgerActor;

/// How long a starting ledger waits for the log to be let go of: longer than a replaced credentials agent's drain and
/// grace period (50 s and 60 s), after which its process has ended and the operating system has let go of its lock.
pub const LEDGER_LOCK_WAIT: Duration = Duration::from_secs(90);

/// Opens the event log at `path`, waiting up to `wait` (asking again every `every`) while another holder has it: a
/// replaced credentials agent still draining, whose appends would fork the log's chain if this one appended too.
pub async fn open_log(
    path: &str,
    wait: Duration,
    every: Duration,
) -> Result<(EventLog<CredentialsEvent>, Vec<CredentialsEvent>), String> {
    let since = std::time::Instant::now();
    let mut said = false;
    loop {
        match EventLog::open(path) {
            Ok(opened) => return Ok(opened),
            Err(polar::event_log::EventLogError::Held { .. }) if since.elapsed() < wait => {
                if !said {
                    tracing::warn!(
                        "the event log at {path} is held by another process (a credentials agent still draining?); \
                         waiting up to {wait:?} for it to be let go of"
                    );
                    said = true;
                }
                tokio::time::sleep(every).await;
            }
            Err(e) => return Err(format!("Failed to open event log at {path}: {e}")),
        }
    }
}

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
    /// The identity that stored it is storing it again, with other labels.
    LabelsChanged,
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
        /// What the document does, as the authority model judged it before it was accepted, if it did.
        effect: Option<credentials_agent_common::events::AuthorityEffect>,
        reply: RpcReplyPort<Result<u64, String>>,
    },
    /// Every account certificate not revoked: the persons a change of authority is judged for.
    ActiveAccounts(RpcReplyPort<Vec<crate::state::AccountCertificateRecord>>),
    /// Each account's username and its account ID, which a policy's or groups document's usernames are converted by.
    AccountNames(RpcReplyPort<std::collections::BTreeMap<String, String>>),
    /// The highest sequence number published so far; 0 before any.
    AuthoritySequence(RpcReplyPort<u64>),
    /// The serial of every certificate an explicit revocation named, in order.
    RevokedSerials(RpcReplyPort<Vec<String>>),
    /// The port on which a newly revoked serial is announced.
    RevocationChanges(RpcReplyPort<Arc<OutputPort<RevokedChanged>>>),
    /// The active grant for `identity`, if any.
    CheckGrant {
        identity: String,
        reply: RpcReplyPort<Option<GrantRecord>>,
    },
    /// Whether `(identity, instance_binding)` currently holds a valid,
    /// unrevoked certificate of this type -- the renewal-prevention check
    /// itself. A pod instance may hold one client and one server
    /// certificate, so it is asked per type.
    CheckRenewal {
        identity: String,
        instance_binding: String,
        cert_type: WorkloadCertType,
        reply: RpcReplyPort<bool>,
    },
    /// Where cert-issuer's record has been read to (docs/architecture/cert-issuer-isolation.md, "Step 2"): its log and its
    /// last entry read, or none before any page.
    RecordCursor {
        reply: RpcReplyPort<Option<(String, u64)>>,
    },
    /// Takes a page of cert-issuer's record: its certificates the ledger has not recorded are recorded, each flagged
    /// unexpected when signed on a token alone for an identity not in `bootstrap` (none: no model, so nothing can be
    /// called unexpected), and the cursor moves, in one
    /// append. Nothing is appended for a page that holds nothing new and moves nothing. Replies with the cursor.
    TakeRecordPage {
        page: cert_issuer_common::record::RecordReply,
        bootstrap: Option<std::collections::BTreeSet<String>>,
        reply: RpcReplyPort<Result<Option<(String, u64)>, String>>,
    },
    /// The current, non-revoked certificate for `identity`, if any --
    /// regardless of instance_binding. Backs the certificate-lookup
    /// query: one agent asking for another's public cert so it can
    /// encrypt a payload directly to it.
    CheckCertificate {
        identity: String,
        reply: RpcReplyPort<Option<CertificateRecord>>,
    },
    /// The certificate `identity` connects with, whose groups policy reads at the request gate: its workload's client
    /// certificate, or else its account's, while that is not revoked (docs/architecture/authority-model.md, slice 9).
    RequesterCertificate {
        identity: String,
        reply: RpcReplyPort<Option<String>>,
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
        let (event_log, events) =
            open_log(&args.log_path, LEDGER_LOCK_WAIT, Duration::from_secs(1))
                .await
                .map_err(ActorProcessingErr::from)?;

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
                        name,
                        stored_by,
                        labels,
                        ..
                    } => state
                        .credentials_state
                        .check_store(name, stored_by, labels)
                        .and_then(|()| {
                            append_and_project(state, &event).map_err(SecretStoreError::Ledger)
                        }),
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
                effect,
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
                        effect,
                    },
                )
                .map(|()| sequence);
                let _ = reply.send(result);
            }
            LedgerMessage::ActiveAccounts(reply) => {
                let _ = reply.send(state.credentials_state.active_account_certificates());
            }
            LedgerMessage::AccountNames(reply) => {
                let _ = reply.send(state.credentials_state.account_names());
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
                cert_type,
                reply,
            } => {
                let has_valid = state.credentials_state.has_valid_certificate(
                    &identity,
                    &instance_binding,
                    cert_type,
                );
                let _ = reply.send(has_valid);
            }
            LedgerMessage::RecordCursor { reply } => {
                let _ = reply.send(state.credentials_state.record_cursor());
            }
            LedgerMessage::TakeRecordPage {
                page,
                bootstrap,
                reply,
            } => {
                let now = polar::time::now_millis();
                let result = match state.credentials_state.reported_page(
                    &page,
                    bootstrap.as_ref(),
                    now,
                ) {
                    None => Ok(()),
                    Some(event) => {
                        if let CredentialsEvent::CertificatesReported { certificates, .. } = &event
                        {
                            for c in certificates.iter().filter(|c| c.unexpected) {
                                tracing::warn!(
                                    identity = %c.identity,
                                    serial = %c.serial,
                                    "cert-issuer signed a certificate on a token alone for an identity that is not a bootstrap service"
                                );
                            }
                        }
                        append_and_project(state, &event)
                    }
                };
                let _ = reply.send(result.map(|()| state.credentials_state.record_cursor()));
            }
            LedgerMessage::CheckCertificate { identity, reply } => {
                let record = state.credentials_state.certificate_for(&identity).cloned();
                let _ = reply.send(record);
            }
            LedgerMessage::RequesterCertificate { identity, reply } => {
                let pem = state
                    .credentials_state
                    .requester_certificate(&identity)
                    .map(str::to_string);
                let _ = reply.send(pem);
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

    #[tokio::test]
    async fn a_starting_ledger_waits_for_the_log_to_be_let_go_of_and_then_opens_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("events.jsonl")
            .to_str()
            .unwrap()
            .to_string();
        let (held, _) = EventLog::<CredentialsEvent>::open(&path).unwrap();
        let letting_go = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            drop(held);
        });
        let since = std::time::Instant::now();
        open_log(&path, Duration::from_secs(5), Duration::from_millis(20))
            .await
            .unwrap();
        assert!(since.elapsed() >= Duration::from_millis(300), "it waited");
        letting_go.await.unwrap();
    }

    #[tokio::test]
    async fn a_ledger_whose_log_is_never_let_go_of_does_not_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("events.jsonl")
            .to_str()
            .unwrap()
            .to_string();
        let (_held, _) = EventLog::<CredentialsEvent>::open(&path).unwrap();
        let why = match open_log(&path, Duration::from_millis(200), Duration::from_millis(20)).await
        {
            Err(why) => why,
            Ok(_) => panic!("opened a log another holder has open"),
        };
        assert!(why.contains("held"), "{why}");
    }

    /// The ledger records `username`'s account certificate, whose first DNS name is `account_id`: a person the
    /// authority knows, by both names.
    pub(crate) async fn holds_account(
        ledger: &ActorRef<LedgerMessage>,
        username: &str,
        account_id: &str,
    ) {
        let key = rcgen::KeyPair::generate().unwrap();
        let certificate = rcgen::CertificateParams::new(vec![account_id.to_string()])
            .unwrap()
            .self_signed(&key)
            .unwrap()
            .pem();
        ractor::call!(ledger, |reply| LedgerMessage::Append {
            event: CredentialsEvent::AccountCertificateIssued {
                account_id: account_id.to_string(),
                username: username.to_string(),
                serial: format!("serial-{username}"),
                timestamp: 1,
                certificate_pem: certificate,
            },
            reply
        })
        .unwrap()
        .unwrap();
    }

    pub(crate) async fn spawn_ledger(log_path: &str) -> ActorRef<LedgerMessage> {
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
                cert_type: WorkloadCertType::Client,
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
                effect: None,
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
                effect: None,
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

    async fn take_page(
        ledger: &ActorRef<LedgerMessage>,
        page: cert_issuer_common::record::RecordReply,
    ) -> Result<Option<(String, u64)>, String> {
        let bootstrap = Some(
            ["cassini.polar.serviceaccount.cluster.local".to_string()]
                .into_iter()
                .collect(),
        );
        ractor::call!(ledger, |reply| LedgerMessage::TakeRecordPage {
            page,
            bootstrap,
            reply
        })
        .unwrap()
    }

    async fn record_cursor(ledger: &ActorRef<LedgerMessage>) -> Option<(String, u64)> {
        ractor::call!(ledger, |reply| LedgerMessage::RecordCursor { reply }).unwrap()
    }

    #[tokio::test]
    async fn a_page_of_cert_issuer_s_record_is_recorded_durably_and_found_after_a_restart() {
        use cert_issuer_common::record::{IssuePath, RecordEntry, RecordReply};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let ledger = spawn_ledger(path.to_str().unwrap()).await;
        assert_eq!(record_cursor(&ledger).await, None);
        let page = RecordReply {
            log: "log-1".to_string(),
            entries: vec![RecordEntry {
                log: "log-1".to_string(),
                seq: 3,
                serial: "c3".to_string(),
                identity: "cassini.polar.serviceaccount.cluster.local".to_string(),
                // A client certificate: the lookup is of a requester's (state.rs, certificate_for).
                cert_type: cert_issuer_common::CertType::Client,
                path: IssuePath::TokenOnly,
                instance_binding: Some("pod".to_string()),
                signed_at: 1000,
                not_after: 5000,
                certificate_pem: "pem".to_string(),
            }],
            more: false,
        };
        assert_eq!(
            take_page(&ledger, page.clone()).await,
            Ok(Some(("log-1".to_string(), 3)))
        );
        let lines = std::fs::read_to_string(&path).unwrap().lines().count();
        // The same page again appends nothing.
        assert_eq!(
            take_page(&ledger, page).await,
            Ok(Some(("log-1".to_string(), 3)))
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            lines
        );
        ledger.stop(None);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let again = spawn_ledger(path.to_str().unwrap()).await;
        assert_eq!(record_cursor(&again).await, Some(("log-1".to_string(), 3)));
        let found = ractor::call!(again, |reply| LedgerMessage::CheckCertificate {
            identity: "cassini.polar.serviceaccount.cluster.local".to_string(),
            reply,
        })
        .unwrap()
        .expect("a reported certificate is found after a restart");
        assert_eq!(found.serial, "c3");
        assert!(found.reported);
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
                cert_type: credentials_agent_common::issuance::WorkloadCertType::Client,
                grant_key: None,
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
                    cert_type: credentials_agent_common::issuance::WorkloadCertType::Client,
                    grant_key: None,
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
