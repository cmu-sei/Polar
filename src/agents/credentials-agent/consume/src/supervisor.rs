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

/// ractor's actor registry is process-global, so any test spawning an
/// actor under LEDGER_ACTOR_NAME or ISSUANCE_ACTOR_NAME -- both
/// hardcoded, since deserialize_and_dispatch requires them by
/// construction -- would otherwise collide nondeterministically with
/// any other such test running concurrently (Rust's default). Every
/// test that registers under these names, or assumes their absence,
/// acquires this lock first.
#[cfg(test)]
static NAMED_ACTOR_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

use crate::ledger::{LedgerActor, LedgerActorArgs, LedgerMessage};
use crate::projection::{ProjectionActor, ProjectionActorArgs, ProjectionMessage};
use crate::identity_normalization::IdentityNormalizationHandler;
use crate::identity_normalization_actor::{IdentityNormalizationActor, IdentityNormalizationActorArgs};
use crate::escrow_key::EscrowKeyHandler;
use crate::escrow_key_actor::{EscrowKeyActor, EscrowKeyActorArgs};
use credentials_agent_common::CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC;
use credentials_agent_common::escrow_key::EscrowKeyRequest;
use cassini_client::{TcpClientMessage, TCPClientConfig, TcpClientActor, TcpClientArgs};
use cassini_types::{ClientEvent,WireTraceCtx};
use credentials_agent_common::{CREDENTIALS_GRANTS_TOPIC, CREDENTIALS_REVOCATIONS_TOPIC};
use credentials_agent_common::events::CredentialsEvent;
use credentials_agent_common::identity::IdentityNormalizationRequest;
use credentials_agent_common::CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC;
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use polar::{Supervisor, SupervisorMessage};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait};
use ractor::registry::where_is;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use crate::credential::configured_kubernetes_issuers;
use crate::introspection::IntrospectionClient;
use crate::issuance::IssuanceHandler;
use crate::issuance_actor::{IssuanceActor, IssuanceActorArgs};
use cert_client::handshake::HandshakeClient;
use credentials_agent_common::CREDENTIALS_ISSUANCE_REQUESTS_TOPIC;
use credentials_agent_common::issuance::IssuanceRequest;
use polar::request_reply::Envelope;

const HEALTHCHECK_ACTOR_NAME: &str = "polar.credentials-agent.healthcheck";
const LEDGER_ACTOR_NAME: &str = "polar.credentials-agent.ledger";
pub const BROKER_CLIENT_NAME: &str = "CREDENTIALS_AGENT_CONSUMER_CLIENT";

/// Path to the ledger's own event log -- the actual source of truth,
/// per credentials-agent-architecture.md. Expected to live on a
/// PersistentVolume, the same way cert-issuer's own CA material is
/// persisted, so "rebuild the log at startup" rebuilds from real
/// history rather than from nothing on every pod restart.
const CREDENTIALS_EVENT_LOG_PATH_ENV: &str = "CREDENTIALS_EVENT_LOG_PATH";
const CREDENTIALS_EVENT_LOG_PATH_DEFAULT: &str = "/var/lib/credentials-agent/events.jsonl";

const CERT_ISSUER_URL_ENV: &str = "CREDENTIALS_AGENT_CERT_ISSUER_URL";
const ISSUANCE_ACTOR_NAME: &str = "polar.credentials-agent.issuance";
const IDENTITY_NORMALIZATION_ACTOR_NAME: &str = "polar.credentials-agent.identity-normalization";
const ESCROW_KEY_ACTOR_NAME: &str = "polar.credentials-agent.escrow-key";

pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    /// Owns the graph projection, and its own GraphControllerActor as
    /// a direct child -- see projection.rs's own module doc. The only
    /// child actor reference genuinely held here: LedgerActor,
    /// IssuanceActor, and IdentityNormalizationActor were previously
    /// also held as fields, but spawn_linked already ties their
    /// lifecycle to this supervisor via ractor's own linking --
    /// nothing in this file ever actually read those fields, and the
    /// compiler was correctly flagging genuine dead code, not a false
    /// positive.
    projection_actor: ActorRef<ProjectionMessage>,
    healthcheck: ActorRef<HealthCheckMessage>,
    draining: bool,
}

impl Supervisor for ConsumerSupervisor {
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>) {
        if topic == CREDENTIALS_ISSUANCE_REQUESTS_TOPIC {
            match rkyv::from_bytes::<Envelope<IssuanceRequest>, rkyv::rancor::Error>(&payload) {
                Ok(envelope) => {
                    if let Some(issuance_actor) = where_is(ISSUANCE_ACTOR_NAME.to_string()) {
                        if let Err(e) = issuance_actor.send_message(envelope) {
                            warn!("Failed to forward issuance request to actor: {e}");
                        }
                    } else {
                        warn!("Issuance actor not yet registered, dropping issuance request");
                    }
                }
                Err(e) => {
                    warn!("Failed to deserialize issuance request from topic {topic}: {e}");
                }
            }
            return;
        }

        if topic == CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC {
            match rkyv::from_bytes::<Envelope<IdentityNormalizationRequest>, rkyv::rancor::Error>(
                &payload,
            ) {
                Ok(envelope) => {
                    if let Some(actor) = where_is(IDENTITY_NORMALIZATION_ACTOR_NAME.to_string()) {
                        if let Err(e) = actor.send_message(envelope) {
                            warn!("Failed to forward identity-normalization request to actor: {e}");
                        }
                    } else {
                        warn!(
                            "Identity-normalization actor not yet registered, dropping request"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to deserialize identity-normalization request from topic {topic}: {e}"
                    );
                }
            }
            return;
        }

        if topic == CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC {
            match rkyv::from_bytes::<Envelope<EscrowKeyRequest>, rkyv::rancor::Error>(&payload) {
                Ok(envelope) => {
                    if let Some(actor) = where_is(ESCROW_KEY_ACTOR_NAME.to_string()) {
                        if let Err(e) = actor.send_message(envelope) {
                            warn!("Failed to forward escrow-key request to actor: {e}");
                        }
                    } else {
                        warn!("Escrow-key actor not yet registered, dropping request");
                    }
                }
                Err(e) => {
                    warn!("Failed to deserialize escrow-key request from topic {topic}: {e}");
                }
            }
            return;
        }

        // Both credentials-grants and credentials-revocations carry
        // the same CredentialsEvent type -- unlike jira/process's
        // convention, topic name isn't used to select a different
        // actor, since there's only ever one writer (LedgerActor)
        // here at all.
        match rkyv::from_bytes::<CredentialsEvent, rkyv::rancor::Error>(&payload) {
            Ok(event) => {
                if let Some(ledger) = where_is(LEDGER_ACTOR_NAME.to_string()) {
                    if let Err(e) = ledger.send_message(LedgerMessage::AppendNoReply(event)) {
                        warn!("Failed to forward event from topic {topic} to ledger: {e}");
                    }
                } else {
                    warn!("Ledger actor not yet registered, dropping event from topic {topic}");
                }
            }
            Err(e) => {
                warn!("Failed to deserialize CredentialsEvent from topic {topic}: {e}");
            }
        }
    }
}

#[async_trait]
impl Actor for ConsumerSupervisor {
    type Msg = SupervisorMessage;
    type State = ConsumerSupervisorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let log_path = std::env::var(CREDENTIALS_EVENT_LOG_PATH_ENV)
            .unwrap_or_else(|_| CREDENTIALS_EVENT_LOG_PATH_DEFAULT.to_string());

        // ProjectionActor owns the graph projection as its own,
        // genuinely separate concern -- see projection.rs's own
        // module doc for why this isn't LedgerActor's job despite
        // being where the events originate. Spawned eagerly, unlike
        // every other graph-writing agent in this codebase -- the
        // Credentials Agent's own health must never depend on Neo4j's
        // availability at all. No technical dependency requires
        // waiting for Cassini registration first; a Bolt connection
        // to Neo4j is entirely independent of Cassini/TCP client
        // registration.
        //
        // Spawned before LedgerActor, with ledger: None -- the two
        // actors need references to each other, which can't both be
        // constructor arguments (neither ActorRef exists before its
        // own actor is spawned). ProjectionActor's need for the
        // ledger reference is deferred via SetLedger below, since
        // nothing calls into it until an actual event or catch-up
        // happens, well after both actors exist.
        let (projection_actor, _) = ProjectionActor::spawn_linked(
            Some("credentials-agent.consumer.projection".to_string()),
            ProjectionActor,
            ProjectionActorArgs { ledger: None },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let (ledger, _) = LedgerActor::spawn_linked(
            Some(LEDGER_ACTOR_NAME.to_string()),
            LedgerActor,
            LedgerActorArgs {
                log_path,
                projection_actor: projection_actor.clone(),
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let _ = projection_actor.cast(ProjectionMessage::SetLedger(ledger.clone()));

        let cert_issuer_url = std::env::var(CERT_ISSUER_URL_ENV).map_err(|_| {
            ActorProcessingErr::from(format!("{CERT_ISSUER_URL_ENV} must be set"))
        })?;

        let escrow_keypair = {
            let escrow_key_path = std::env::var(crate::escrow::CREDENTIALS_ESCROW_KEY_PATH_ENV)
                .unwrap_or_else(|_| {
                    crate::escrow::CREDENTIALS_ESCROW_KEY_PATH_DEFAULT.to_string()
                });
            std::sync::Arc::new(
                crate::escrow::EscrowKeypair::load_or_generate(&escrow_key_path)
                    .map_err(ActorProcessingErr::from)?,
            )
        };

        let issuance_handler = IssuanceHandler {
            ledger: ledger.clone(),
            cert_issuer: HandshakeClient::new(cert_issuer_url.clone()),
            // Introspection is genuinely optional at the deployment
            // level -- a cluster that never has LikelyIdp credentials
            // in practice doesn't need it configured. If one does
            // arrive without introspection configured, the handler
            // itself refuses it (TokenIntrospectionFailed) rather
            // than silently proceeding unintrospected.
            introspection: IntrospectionClient::from_env().ok(),
            known_kubernetes_issuers: configured_kubernetes_issuers(),
            escrow_keypair: escrow_keypair.clone(),
        };

        let (_issuance_actor, _) = IssuanceActor::spawn_linked(
            Some(ISSUANCE_ACTOR_NAME.to_string()),
            IssuanceActor,
            IssuanceActorArgs {
                handler: issuance_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let identity_normalization_handler = IdentityNormalizationHandler {
            cert_issuer: HandshakeClient::new(cert_issuer_url.clone()),
        };

        let (_identity_normalization_actor, _) = IdentityNormalizationActor::spawn_linked(
            Some(IDENTITY_NORMALIZATION_ACTOR_NAME.to_string()),
            IdentityNormalizationActor,
            IdentityNormalizationActorArgs {
                handler: identity_normalization_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let escrow_key_handler = EscrowKeyHandler {
            escrow_keypair: escrow_keypair.clone(),
        };
        let (_escrow_key_actor, _) = EscrowKeyActor::spawn_linked(
            Some(ESCROW_KEY_ACTOR_NAME.to_string()),
            EscrowKeyActor,
            EscrowKeyActorArgs {
                handler: escrow_key_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let prepare_shutdown_port = Arc::new(OutputPort::<()>::default());

        prepare_shutdown_port.subscribe(myself.clone(), |()| {
            Some(SupervisorMessage::PrepareShutdown)
        });

        let dep_cert_endpoints = vec![
            DepCertEndpoint::parse("cassini-ip-svc.polar.svc.cluster.local:8080:300")
                .map_err(ActorProcessingErr::from)?,
            DepCertEndpoint::parse("polar-db-svc.polar-graph.svc.cluster.local:7687:300")
                .map_err(ActorProcessingErr::from)?,
        ];

        let (healthcheck, _) = HealthCheckActor::spawn_linked(
            Some(HEALTHCHECK_ACTOR_NAME.to_string()),
            HealthCheckActor,
            HealthCheckArgs {
                expects_graph: false,
                rejuvenation_threshold_secs: 300,
                dep_cert_endpoints,
                prepare_shutdown_port,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let events_output = Arc::new(OutputPort::default());
        events_output.subscribe(myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        });

        let client_config = TCPClientConfig::new()?;

        let (_client, _) = Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config: client_config,
                registration_id: None,
                events_output: Some(events_output),
                event_handler: None,
            },
            myself.clone().into(),
        )
        .await?;

        Ok(ConsumerSupervisorState {
            projection_actor,
            healthcheck,
            draining: false,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::Heartbeat => {}
            SupervisorMessage::PrepareShutdown => {
                info!("CONSUMER_SUPERVISOR: PrepareShutdown received");
                state.draining = true;
            }
            SupervisorMessage::GraphSignal(_) => {
                // Nothing subscribes to produce this anymore -- the
                // graph controller is spawned eagerly with no signal
                // consumer, since this agent's health never depends
                // on graph availability. Kept as an explicit no-op
                // arm, not a wildcard, so this stays visible and
                // intentional rather than silently swallowed by a
                // catch-all.
            }
            SupervisorMessage::ForceExit => {
                warn!("CONSUMER_SUPERVISOR: drain window elapsed, beginning graceful shutdown");
                // ProjectionActor owns its own GraphControllerActor as
                // a direct, linked child now -- stopping it cascades
                // down to stop the graph controller cleanly too,
                // rather than the supervisor needing its own separate
                // reference to reach it directly.
                warn!(
                    "CONSUMER_SUPERVISOR: stopping projection actor (and its graph controller) \
                     and waiting for any in-flight write to complete before exiting"
                );
                match state
                    .projection_actor
                    .stop_and_wait(
                        Some("supervisor shutting down".into()),
                        Some(std::time::Duration::from_secs(30)),
                    )
                    .await
                {
                    Ok(()) => info!("CONSUMER_SUPERVISOR: projection actor stopped cleanly"),
                    Err(e) => error!(
                        "CONSUMER_SUPERVISOR: projection actor did not stop cleanly within \
                         30s, exiting anyway: {e:?}"
                    ),
                }
                warn!("CONSUMER_SUPERVISOR: exiting now");
                std::process::exit(1);
            }
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);
                    // Graph controller is spawned eagerly in pre_start now, not here -- this
                    // agent's own health never depends on graph availability at all (expects_graph:
                    // false), so there's no GraphConnected signal to cast, and no lazy
                    // spawn-on-Registered logic needed. This is different from most every other
                    // consumer in Polar, since Credentials Agent's source of truth is its own
                    // event-sourced log and the bits that end up in the graph are simply a state
                    // projection.
                    if let Some(client) = where_is(BROKER_CLIENT_NAME.to_string()) {
                        for topic in [
                            CREDENTIALS_GRANTS_TOPIC,
                            CREDENTIALS_REVOCATIONS_TOPIC,
                            CREDENTIALS_ISSUANCE_REQUESTS_TOPIC,
                            CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC,
                            CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC,
                        ] {
                            if let Err(e) = client.send_message(TcpClientMessage::Subscribe {
                                topic: topic.to_string(),
                                trace_ctx: WireTraceCtx::from_current_span(),
                            }) {
                                error!("Failed to subscribe to {topic}: {e}");
                            }
                        }
                    } else {
                        error!("Broker client not found while attempting to subscribe");
                    }
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    if state.draining {
                        warn!(
                            "CONSUMER_SUPERVISOR: draining -- logging message on topic \
                             '{topic}' ({} bytes) instead of dispatching; will be lost on \
                             exit",
                            payload.len()
                        );
                        // TODO: publish to dead-letter queue once Cassini supports one.
                    } else {
                        ConsumerSupervisor::deserialize_and_dispatch(topic, payload);
                    }
                }
                _ => {}
            },
        }
        Ok(())
    } 

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        message: SupervisionEvent,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisionEvent::ActorTerminated(actor, _, reason) => {
                let name = actor.get_name();
                if name.as_deref() == Some(HEALTHCHECK_ACTOR_NAME) {
                    info!("Healthcheck actor terminated ({reason:?}), exiting");
                    myself.stop(None);
                } else if name.as_deref() == Some(LEDGER_ACTOR_NAME) {
                    error!("Ledger actor terminated ({reason:?}) -- exiting, since the ledger is this agent's actual source of truth and nothing else can safely continue without it");
                    myself.stop(None);
                }
            }
            SupervisionEvent::ActorFailed(actor, err) => {
                error!("Child actor {actor:?} failed: {err}");
                myself.stop(None);
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::GrantRecord;

    async fn spawn_ledger_registered(log_path: &str) -> ActorRef<LedgerMessage> {
        let (ledger, _) = Actor::spawn(
            Some(LEDGER_ACTOR_NAME.to_string()),
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

    async fn check_grant(ledger: &ActorRef<LedgerMessage>, identity: &str) -> Option<GrantRecord> {
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

    #[tokio::test]
    async fn deserialize_and_dispatch_forwards_a_real_event_to_the_real_ledger() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let ledger = spawn_ledger_registered(path.to_str().unwrap()).await;

        let event = CredentialsEvent::Grant {
            target_identity: "alice".to_string(),
            permissions: vec!["read".to_string()],
            granted_by: "admin".to_string(),
            timestamp: 1000,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&event)
            .unwrap()
            .to_vec();

        // The real function every incoming Cassini message actually
        // goes through -- not a reimplementation of it.
        ConsumerSupervisor::deserialize_and_dispatch(
            CREDENTIALS_GRANTS_TOPIC.to_string(),
            bytes,
        );

        // A subsequent call() to the same actor only returns after
        // every prior message (including our dispatch above) has
        // already been processed, by ractor's own sequential-mailbox
        // guarantee -- no arbitrary sleep needed to synchronize.
        let grant = check_grant(&ledger, "alice").await;
        assert!(grant.is_some());
        assert_eq!(grant.unwrap().granted_by, "admin");

        ledger.stop_and_wait(None, None).await.unwrap();
    }

    #[tokio::test]
    async fn deserialize_and_dispatch_handles_missing_ledger_gracefully() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;
        // No ledger registered at all -- must not panic, just log and
        // drop, matching the documented behavior for this case.
        let event = CredentialsEvent::Revoke {
            serial: "abc123".to_string(),
            reason: "compromised".to_string(),
            revoked_by: "admin".to_string(),
            timestamp: 1000,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&event)
            .unwrap()
            .to_vec();

        ConsumerSupervisor::deserialize_and_dispatch(
            CREDENTIALS_REVOCATIONS_TOPIC.to_string(),
            bytes,
        );
        // Reaching this line at all is the assertion -- no panic.
    }
}

#[cfg(test)]
mod issuance_wiring_tests {
    use super::*;
    use crate::introspection::{
        INTROSPECTION_CLIENT_ID_ENV, INTROSPECTION_CLIENT_SECRET_ENV, INTROSPECTION_ENDPOINT_ENV,
    };
    use crate::state::GrantRecord;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_token(iss: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"iss":"{iss}"}}"#));
        format!("{header}.{payload}.")
    }

    async fn check_grant(ledger: &ActorRef<LedgerMessage>, identity: &str) -> Option<GrantRecord> {
        match ractor::rpc::call(
            ledger,
            |reply| LedgerMessage::CheckGrant {
                identity: identity.to_string(),
                reply,
            },
            None,
        )
        .await
        .unwrap()
        {
            ractor::rpc::CallResult::Success(result) => result,
            other => panic!("expected Success, got {other:?}"),
        }
    }

    async fn check_renewal(ledger: &ActorRef<LedgerMessage>, identity: &str, instance_binding: &str) -> bool {
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
        .unwrap()
        {
            ractor::rpc::CallResult::Success(result) => result,
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn full_pipeline_from_raw_bytes_through_dispatch_to_a_recorded_issuance() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;
        // Real, end-to-end proof: raw bytes arriving on
        // CREDENTIALS_ISSUANCE_REQUESTS_TOPIC -> deserialize_and_dispatch
        // -> IssuanceActor's real async handle() -> IssuanceHandler's
        // real logic (classify, validate, grant/renewal checks, sign)
        // -> a genuinely recorded IssuanceCompleted event in the ledger.
        // No shortcuts -- every real component in the actual chain.

        // No introspection needed for this scenario (Kubernetes credential).
        unsafe {
            std::env::remove_var(INTROSPECTION_ENDPOINT_ENV);
            std::env::remove_var(INTROSPECTION_CLIENT_ID_ENV);
            std::env::remove_var(INTROSPECTION_CLIENT_SECRET_ENV);
        }

        let cert_issuer = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "system:serviceaccount:polar:admin-tool".to_string(),
                    instance_binding: Some("admin-tool-uid".to_string()),
                    normalized_identity: None,
                },
            ))
            .mount(&cert_issuer)
            .await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::IssueResponse {
                    certificate_pem: "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
                        .to_string(),
                    ca_chain_pem: "-----BEGIN CERTIFICATE-----\nfake-ca\n-----END CERTIFICATE-----\n"
                        .to_string(),
                    serial_number: "full-pipeline-test-serial".to_string(),
                    session_id: "00000000-0000-0000-0000-000000000099".to_string(),
                    expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
                },
            ))
            .mount(&cert_issuer)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.jsonl");
        let (ledger, _) = LedgerActor::spawn(
            Some(LEDGER_ACTOR_NAME.to_string()),
            LedgerActor,
            LedgerActorArgs {
                log_path: log_path.to_str().unwrap().to_string(),
                projection_actor: crate::projection::tests::fake_projection_actor().await,
            },
        )
        .await
        .unwrap();

        let handler = IssuanceHandler {
            ledger: ledger.clone(),
            cert_issuer: HandshakeClient::new(cert_issuer.uri()),
            introspection: None,
            known_kubernetes_issuers: vec!["https://kubernetes.default.svc.cluster.local".to_string()],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let (issuance_actor, _) = IssuanceActor::spawn(
            Some(ISSUANCE_ACTOR_NAME.to_string()),
            IssuanceActor,
            IssuanceActorArgs { handler },
        )
        .await
        .unwrap();

        let request = IssuanceRequest {
            csr_pem: "fake-csr".to_string(),
            credential: make_token("https://kubernetes.default.svc.cluster.local"),
        };
        let envelope = Envelope {
            correlation_id: "test-correlation-id".to_string(),
            payload: request,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
            .unwrap()
            .to_vec();

        // The real function every incoming Cassini message on this
        // topic actually goes through.
        ConsumerSupervisor::deserialize_and_dispatch(
            CREDENTIALS_ISSUANCE_REQUESTS_TOPIC.to_string(),
            bytes,
        );

        // IssuanceActor processes on its own independent mailbox, so
        // (unlike LedgerActor's own sequential-mailbox synchronization
        // trick) there's no single deterministic checkpoint to await
        // here -- a short sleep is a real, honest compromise, not a
        // fully deterministic wait.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        assert!(
            check_renewal(&ledger, "system:serviceaccount:polar:admin-tool", "admin-tool-uid").await,
            "IssuanceCompleted must have actually been recorded in the ledger by now"
        );
        assert!(check_grant(&ledger, "system:serviceaccount:polar:admin-tool")
            .await
            .is_none());

        issuance_actor.stop_and_wait(None, None).await.unwrap();
        ledger.stop_and_wait(None, None).await.unwrap();
    }
}

#[cfg(test)]
mod identity_normalization_wiring_tests {
    use super::*;
    use crate::identity_normalization::IdentityNormalizationHandler;
    use crate::identity_normalization_actor::{
        IdentityNormalizationActor, IdentityNormalizationActorArgs,
    };
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn full_pipeline_from_raw_bytes_through_dispatch_reaches_cert_issuer() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;

        let cert_issuer = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "testuser".to_string(),
                    instance_binding: None,
                    normalized_identity: Some("testuser.polar.keycloak-issuer".to_string()),
                },
            ))
            .mount(&cert_issuer)
            .await;

        let handler = IdentityNormalizationHandler {
            cert_issuer: HandshakeClient::new(cert_issuer.uri()),
        };

        let (_actor, _) = IdentityNormalizationActor::spawn(
            Some(IDENTITY_NORMALIZATION_ACTOR_NAME.to_string()),
            IdentityNormalizationActor,
            IdentityNormalizationActorArgs { handler },
        )
        .await
        .unwrap();

        let request = IdentityNormalizationRequest {
            credential: "any-token".to_string(),
        };
        let envelope = Envelope {
            correlation_id: "test-correlation-id".to_string(),
            payload: request,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
            .unwrap()
            .to_vec();

        // The real function every incoming Cassini message on this
        // topic actually goes through.
        ConsumerSupervisor::deserialize_and_dispatch(
            CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC.to_string(),
            bytes,
        );

        // The actor processes on its own independent mailbox -- same
        // honest compromise as issuance's own full-pipeline test, a
        // short sleep rather than a fully deterministic wait.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // No real broker is registered in this test, so the actor's
        // final publish-the-reply step is expected to fail/log (no
        // BROKER_CLIENT_NAME actor exists here) -- what this proves
        // instead is that the full chain up to that point genuinely
        // ran: dispatch -> actor -> handler -> a real HTTP call to
        // cert-issuer's /validate, confirmed by the mock's own
        // request count, not just assumed from no panic occurring.
        let received = cert_issuer.received_requests().await.unwrap();
        assert_eq!(received.len(), 1, "cert-issuer's /validate must have been called exactly once");
    }
}
