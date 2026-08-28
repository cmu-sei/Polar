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
use cassini_client::{TcpClientMessage, TCPClientConfig, TcpClientActor, TcpClientArgs};
use cassini_types::{ClientEvent,WireTraceCtx};
use credentials_agent_common::{CREDENTIALS_GRANTS_TOPIC, CREDENTIALS_REVOCATIONS_TOPIC};
use credentials_agent_common::events::CredentialsEvent;
use polar::graph::controller::{GraphController, GraphControllerActor, GraphControllerArgs, GraphSignal};
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

pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    /// The single, serialized owner of the event log and derived
    /// state -- see ledger.rs's own module doc for why this can't be
    /// split across multiple actors the way jira/process's four data
    /// streams safely are.
    ledger: ActorRef<LedgerMessage>,
    issuance_actor: ActorRef<Envelope<IssuanceRequest>>,
    graph_controller: Option<GraphController>,
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

        let (ledger, _) = LedgerActor::spawn_linked(
            Some(LEDGER_ACTOR_NAME.to_string()),
            LedgerActor,
            LedgerActorArgs { log_path },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let cert_issuer_url = std::env::var(CERT_ISSUER_URL_ENV).map_err(|_| {
            ActorProcessingErr::from(format!("{CERT_ISSUER_URL_ENV} must be set"))
        })?;

        let issuance_handler = IssuanceHandler {
            ledger: ledger.clone(),
            cert_issuer: HandshakeClient::new(cert_issuer_url),
            // Introspection is genuinely optional at the deployment
            // level -- a cluster that never has LikelyIdp credentials
            // in practice doesn't need it configured. If one does
            // arrive without introspection configured, the handler
            // itself refuses it (TokenIntrospectionFailed) rather
            // than silently proceeding unintrospected.
            introspection: IntrospectionClient::from_env().ok(),
            known_kubernetes_issuers: configured_kubernetes_issuers(),
        };

        let (issuance_actor, _) = IssuanceActor::spawn_linked(
            Some(ISSUANCE_ACTOR_NAME.to_string()),
            IssuanceActor,
            IssuanceActorArgs {
                handler: issuance_handler,
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
                expects_graph: true,
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
            ledger,
            issuance_actor,
            graph_controller: None,
            healthcheck,
            draining: false,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::Heartbeat => {}
            SupervisorMessage::PrepareShutdown => {
                info!("CONSUMER_SUPERVISOR: PrepareShutdown received");
                state.draining = true;
            }
            SupervisorMessage::GraphSignal(signal) => match signal {
                GraphSignal::Available => {
                    let _ = state.healthcheck.cast(HealthCheckMessage::GraphAvailable);
                }
                GraphSignal::OpFailed(reason) => {
                    let _ = state
                        .healthcheck
                        .cast(HealthCheckMessage::GraphOpFailed(reason));
                }
            },
            SupervisorMessage::ForceExit => {
                warn!("CONSUMER_SUPERVISOR: drain window elapsed, beginning graceful shutdown");

                if let Some(controller) = state.graph_controller.take() {
                    warn!(
                        "CONSUMER_SUPERVISOR: stopping graph controller and waiting for any \
                         in-flight write to complete before exiting"
                    );
                    match controller
                        .stop_and_wait(
                            Some("supervisor shutting down".into()),
                            Some(std::time::Duration::from_secs(30)),
                        )
                        .await
                    {
                        Ok(()) => info!("CONSUMER_SUPERVISOR: graph controller stopped cleanly"),
                        Err(e) => error!(
                            "CONSUMER_SUPERVISOR: graph controller did not stop cleanly \
                             within 30s, exiting anyway: {e:?}"
                        ),
                    }
                }

                warn!("CONSUMER_SUPERVISOR: exiting now");
                std::process::exit(1);
            }
            SupervisorMessage::ClientEvent { event } => {
                if let ClientEvent::Registered { .. } = event {
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);

                    if state.graph_controller.is_none() {
                        let graph_signal_port = Arc::new(OutputPort::<GraphSignal>::default());
                        graph_signal_port.subscribe(myself.clone(), |signal| {
                            Some(SupervisorMessage::GraphSignal(signal))
                        });

                        match GraphControllerActor::spawn_linked(
                            Some("credentials-agent.consumer.graph.controller".to_string()),
                            GraphControllerActor,
                            GraphControllerArgs {
                                signal_port: graph_signal_port,
                            },
                            myself.get_cell(),
                        )
                        .await
                        {
                            Ok((graph_controller, _)) => {
                                state.graph_controller = Some(graph_controller);
                                let _ =
                                    state.healthcheck.cast(HealthCheckMessage::GraphConnected);
                            }
                            Err(e) => {
                                error!("Error initializing graph controller! {e}");
                                return Err(ActorProcessingErr::from(e));
                            }
                        }
                    } else {
                        let _ = state.healthcheck.cast(HealthCheckMessage::GraphConnected);
                    }

                    if let Some(client) = where_is(BROKER_CLIENT_NAME.to_string()) {
                        for topic in [
                            CREDENTIALS_GRANTS_TOPIC,
                            CREDENTIALS_REVOCATIONS_TOPIC,
                            CREDENTIALS_ISSUANCE_REQUESTS_TOPIC,
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
            }
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
            },
        )
        .await
        .unwrap();

        let handler = IssuanceHandler {
            ledger: ledger.clone(),
            cert_issuer: HandshakeClient::new(cert_issuer.uri()),
            introspection: None,
            known_kubernetes_issuers: vec!["https://kubernetes.default.svc.cluster.local".to_string()],
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

        _issuance_actor.stop_and_wait(None, None).await.unwrap();
        ledger.stop_and_wait(None, None).await.unwrap();
    }
}
