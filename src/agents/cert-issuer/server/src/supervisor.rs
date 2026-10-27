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

//! cert-issuer's supervisor, on the agent builder (docs/architecture/cert-issuer-isolation.md, "1b in detail"). It is the
//! agent's one domain supervisor: it loads the CA, signs cert-issuer its own client certificate through its own floor,
//! builds the agent (health, router, the Cassini front: cert-issuer speaks over Cassini without depending on it), and
//! runs the listener as a linked actor.
//!
//! Shutdown, in order, for whatever reason the agent is to end:
//!
//! 1. `InfrastructureEvent::PrepareShutdown` reaches this supervisor.
//! 2. The listener is drained: no new connections, the ones in flight finished within its timeout.
//! 3. `ProceedWithShutdown` goes to the infrastructure supervisor, which stops its children.
//! 4. The infrastructure supervisor's ending stops this supervisor, so the agent's `start` returns and `run` exits
//!    non-zero: a fresh pod starts with fresh certificates.
//!
//! A listener that ends any other way begins the shutdown, so a CA front that serves nothing cannot look healthy.

use crate::ca::{RcgenCaClient, load_or_bootstrap_ca, load_provisioned_ca};
use crate::config::{CaConfig, CaProvisioning};
use crate::floor_state::{FloorState, Heard};
use crate::gate::FloorGate;
use crate::handler::Handler;
use crate::listener::{Listener, ListenerArgs, ListenerMessage};
use crate::oidc::{HttpJwksSource, Validator};
use crate::own_certificate::issue_own;
use crate::record::{Record, RecordingCa};
use crate::server::build_router;
use crate::settings::Settings;
use crate::tls::build_server_config;
use cassini_types::ClientEvent;
use cert_issuer_common::held::{HELD_REPLIES_TOPIC, HELD_REQUESTS_TOPIC, HeldOffer, HeldReply};
use polar_agent_builder::{
    AgentBuilder, Configured, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage,
};
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

pub const SERVICE_NAME: &str = "cert-issuer";

/// The drain gets half; the rest is the infrastructure's.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(20);

/// How long a drain waits for the connections in flight. An issuance is one request and a few signatures.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

pub enum CertIssuerSupervisorMessage {
    ClientEvent {
        event: ClientEvent,
    },
    PrepareShutdown,
    /// A request for the record arrived, published (the broker says) by `published_by`.
    RecordRequested {
        payload: Vec<u8>,
        published_by: Option<String>,
    },
    /// Held requests to offer to the credentials agent over Cassini.
    OfferHeld(Vec<HeldOffer>),
    /// An answer to a held request arrived, published (the broker says) by `published_by`.
    HeldAnswered {
        payload: Vec<u8>,
        published_by: Option<String>,
    },
}

pub struct CertIssuerSupervisor;

pub struct CertIssuerSupervisorState {
    healthcheck: HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    listener: Option<ActorRef<ListenerMessage>>,
    /// The listener's id, kept after the drain takes the listener, to know its ending.
    listener_id: Option<ractor::ActorId>,
    /// The shutdown has begun: a listener ending now is the drain's doing.
    shutting_down: bool,
    /// The record, answered a page at a time to the credentials agent alone.
    record: Arc<RecordingCa>,
    internal_callers: Vec<String>,
    client: polar::cassini::TcpClient,
    /// The floor's gate, told what this supervisor hears and given the credentials agent's answers.
    gate: Arc<FloorGate>,
}

/// What a child's ending means: the infrastructure's is the end of the shutdown; the listener's, outside a shutdown,
/// begins one, so a CA front that serves nothing cannot look healthy, and during one it is the drain's doing.
pub fn on_child_ended(child: Child, shutting_down: bool, why: &str) -> Ending {
    match child {
        Child::Infrastructure => Ending::Stop,
        Child::Listener if !shutting_down => {
            Ending::BeginShutdown(format!("the listener ended outside a shutdown ({why})"))
        }
        Child::Listener | Child::Other => Ending::Nothing,
    }
}

/// Which child `ended` is, given the infrastructure's id and the listener's.
pub fn child_of(
    ended: ractor::ActorId,
    infrastructure: ractor::ActorId,
    listener: Option<ractor::ActorId>,
) -> Child {
    if ended == infrastructure {
        Child::Infrastructure
    } else if listener == Some(ended) {
        Child::Listener
    } else {
        Child::Other
    }
}

/// One of this supervisor's children, as its ending is judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Child {
    Infrastructure,
    Listener,
    Other,
}

/// What this supervisor does when a child ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    /// The infrastructure has finished the shutdown: stop, so the agent's `start` returns and `run` exits non-zero.
    Stop,
    /// Ask the infrastructure for an orderly shutdown, for this reason.
    BeginShutdown(String),
    Nothing,
}

/// The CA's certificate and key, as `ca` says to come by them: only loaded when provisioned, made on the first start
/// when self-bootstrapped.
fn ca_materials(ca: &CaConfig) -> Result<(String, String), String> {
    match ca.provisioning {
        // An operator made it ahead of time: a missing CA is an error, never a reason to make a new root.
        CaProvisioning::Provisioned => load_provisioned_ca(&ca.ca_cert_path, &ca.ca_key_path)
            .map_err(|e| format!("the provisioned CA: {e}")),
        // Loaded CAs keep whatever name they were made with.
        CaProvisioning::SelfBootstrap => {
            load_or_bootstrap_ca(&ca.ca_cert_path, &ca.ca_key_path, "Polar Internal CA")
                .map_err(|e| format!("the CA: {e}"))
        }
    }
}

#[ractor::async_trait]
impl Actor for CertIssuerSupervisor {
    type Msg = CertIssuerSupervisorMessage;
    type State = CertIssuerSupervisorState;
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        let settings = Settings::from_env(configured.config().as_ref())
            .map_err(|problems| ActorProcessingErr::from(problems.join("; ")))?;
        let config = &settings.service;
        info!(
            bind_addr = %config.bind_addr,
            issuers = config.issuers.len(),
            "cert-issuer starting"
        );

        let (ca_cert_pem, ca_key_pem) = ca_materials(&config.ca)?;
        // Every certificate it signs is recorded before it is returned, its own among them, so that no certificate
        // it signs stays unknown to the credentials agent (docs/architecture/cert-issuer-isolation.md, "Step 2").
        let record = Record::open(std::path::Path::new(&settings.record_path))?;
        // A fresh cluster is a record with nothing in it but cert-issuer's own certificates, read before it signs any
        // of its own now (docs/architecture/core-restarts.md, "2 in detail").
        let fresh = record
            .entries()
            .iter()
            .all(|e| e.path == cert_issuer_common::record::IssuePath::Own);
        let offers = myself.clone();
        let gate = Arc::new(FloorGate::new(
            FloorState::new(fresh, std::time::Instant::now()),
            Box::new(move |held| {
                let _ = offers.send_message(CertIssuerSupervisorMessage::OfferHeld(held));
            }),
        ));
        info!(
            fresh,
            "floor state: {}",
            if fresh {
                "a fresh cluster, bootstrap until the credentials agent is first heard"
            } else {
                "a start with a record: nothing opens until the credentials agent is heard or the bound passes"
            }
        );
        let ca = Arc::new(RecordingCa::new(
            Arc::new(
                RcgenCaClient::new(&ca_cert_pem, &ca_key_pem)
                    .map_err(|e| format!("the CA client: {e}"))?,
            ),
            record,
        ));
        // Its own TLS server certificate, from its own CA, recorded like every other.
        let tls = build_server_config(ca.as_ref(), &ca_cert_pem, &config.mtls.server_identity)
            .await
            .map_err(|e| format!("the TLS server configuration: {e}"))?;
        let validator = Validator::with_jwks_source(
            config.issuers.clone(),
            Arc::new(HttpJwksSource::with(
                settings.kubernetes_ca_cert.as_deref(),
                &settings.kubernetes_sa_token,
            )),
        );
        let handler = Arc::new(Handler {
            validator: Arc::new(validator),
            ca: ca.clone(),
            default_lifetime: config.ca.default_lifetime,
            server_lifetime: config.ca.server_lifetime,
            user_lifetime: config.ca.user_lifetime,
            service_lifetime: config.ca.service_lifetime,
            own_workload: Default::default(),
            gate: Some(gate.clone()),
            identity_lifetime_overrides: config.ca.identity_lifetime_overrides.clone(),
            internal_callers: config.mtls.internal_callers.clone(),
            floor: config.mtls.bootstrap.clone(),
            ca_cert_pem,
        });
        match &config.mtls.bootstrap {
            Some(floor) => info!(
                bootstrap_identities = floor.len(),
                "issuance floor configured: a caller with only a token must be a bootstrap identity"
            ),
            None => warn!(
                "issuance floor NOT configured: /issue and /validate are open to any holder of a valid \
                 service-account token, who may ask for any certificate type and names. Set CERT_ISSUER_BOOTSTRAP."
            ),
        }

        // Its own client certificate, and the CA it is signed under, where its Cassini client reads them, before the
        // builder checks that the certificate names this agent.
        let token = std::fs::read_to_string(&settings.own_token)
            .map_err(|e| format!("its own token, {}: {e}", settings.own_token))?;
        let own = issue_own(&handler, token.trim()).await?;
        let certificate = configured
            .config()
            .read(&cassini_client::environment::TLS_CLIENT_CERT)
            .map_err(|e| e.to_string())?;
        let key = configured
            .config()
            .read(&cassini_client::environment::TLS_CLIENT_KEY)
            .map_err(|e| e.to_string())?;
        let ca_file = configured
            .config()
            .read(&cassini_client::environment::TLS_CA_CERT)
            .map_err(|e| e.to_string())?;
        own.write(
            Path::new(&certificate),
            Path::new(&key),
            Path::new(&ca_file),
        )?;
        info!("signed its own client certificate through its floor");

        let asked = myself.clone();
        let answered = myself.clone();
        let resources = AgentBuilder::new(myself.clone(), SERVICE_NAME)
            .with_lifecycle_handler(|event| {
                Some(CertIssuerSupervisorMessage::ClientEvent { event })
            })
            .with_subscription(polar_agent_builder::Subscription::new(
                cert_issuer_common::record::RECORD_REQUESTS_TOPIC,
                move |incoming| {
                    let _ = asked.send_message(CertIssuerSupervisorMessage::RecordRequested {
                        payload: incoming.payload,
                        published_by: incoming.published_by,
                    });
                },
            ))
            .with_subscription(polar_agent_builder::Subscription::new(
                HELD_REPLIES_TOPIC,
                move |incoming| {
                    let _ = answered.send_message(CertIssuerSupervisorMessage::HeldAnswered {
                        payload: incoming.payload,
                        published_by: incoming.published_by,
                    });
                },
            ))
            .into_configured(configured)
            .build()
            .await?;
        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => {
                    Some(CertIssuerSupervisorMessage::PrepareShutdown)
                }
            });

        let tcp = tokio::net::TcpListener::bind(&config.bind_addr)
            .await
            .map_err(|e| format!("cannot listen on {}: {e}", config.bind_addr))?;
        let (listener, _) = Actor::spawn_linked(
            Some(format!("{SERVICE_NAME}.listener")),
            Listener,
            ListenerArgs::tls(
                tcp,
                tokio_rustls::TlsAcceptor::from(tls),
                build_router(handler),
                DRAIN_TIMEOUT,
            ),
            myself.get_cell(),
        )
        .await?;
        info!(bind_addr = %config.bind_addr, "listening (TLS; client certificate optional)");

        Ok(CertIssuerSupervisorState {
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            listener_id: Some(listener.get_id()),
            listener: Some(listener),
            shutting_down: false,
            record: ca,
            internal_callers: config.mtls.internal_callers.clone(),
            client: resources.tcp_client,
            gate,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            // Reported, never a reason to end: its health does not count the session.
            CertIssuerSupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cassini_connected();
                    state
                        .gate
                        .hear(Heard::CassiniRegistered, std::time::Instant::now());
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Cassini is unreachable (cert-issuer does not depend on it): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                    state
                        .gate
                        .hear(Heard::CassiniLost, std::time::Instant::now());
                }
                _ => {}
            },
            CertIssuerSupervisorMessage::RecordRequested {
                payload,
                published_by,
            } => {
                // The credentials agent asking, as the broker verified it: that is its presence.
                if published_by
                    .as_deref()
                    .is_some_and(|asker| state.internal_callers.iter().any(|c| c == asker))
                {
                    let now = std::time::Instant::now();
                    state.gate.hear(Heard::CredentialsAgent, now);
                    // Offered again, so a held request lost on the way is not lost for good.
                    state.gate.offer_waiting(now);
                }
                match state
                    .record
                    .answer(&payload, published_by.as_deref(), &state.internal_callers)
                    .await
                {
                    Ok(reply) => {
                        if let Err(e) = state.client.as_actor_ref().send_message(
                            cassini_client::TcpClientMessage::publish(
                                cert_issuer_common::record::RECORD_REPLIES_TOPIC.to_string(),
                                reply,
                                None,
                            ),
                        ) {
                            warn!("the record could not be sent: {e}");
                        }
                    }
                    Err(why) => warn!("not answered: {why}"),
                }
            }
            CertIssuerSupervisorMessage::OfferHeld(held) => {
                for offer in held {
                    let payload = match serde_json::to_vec(&offer) {
                        Ok(payload) => payload,
                        Err(e) => {
                            warn!("a held request could not be written: {e}");
                            continue;
                        }
                    };
                    if let Err(e) = state.client.as_actor_ref().send_message(
                        cassini_client::TcpClientMessage::publish(
                            HELD_REQUESTS_TOPIC.to_string(),
                            payload,
                            None,
                        ),
                    ) {
                        warn!("a held request could not be offered: {e}");
                    }
                }
            }
            CertIssuerSupervisorMessage::HeldAnswered {
                payload,
                published_by,
            } => {
                // An answer counts only from the credentials agent, as the broker verified it.
                if !published_by
                    .as_deref()
                    .is_some_and(|by| state.internal_callers.iter().any(|c| c == by))
                {
                    warn!(published_by = ?published_by, "an answer to a held request from someone other than the credentials agent: ignored");
                    return Ok(());
                }
                match serde_json::from_slice::<HeldReply>(&payload) {
                    Ok(reply) => {
                        state
                            .gate
                            .hear(Heard::CredentialsAgent, std::time::Instant::now());
                        let taken = state.gate.answer(&reply);
                        info!(held = %reply.id, decision = ?reply.decision, taken, "the credentials agent answered a held request");
                    }
                    Err(e) => warn!("an answer to a held request that does not read: {e}"),
                }
            }
            CertIssuerSupervisorMessage::PrepareShutdown => {
                info!("PrepareShutdown received: draining the listener before infrastructure");
                state.shutting_down = true;
                if let Some(listener) = state.listener.take()
                    && let Err(e) = ractor::call!(listener, ListenerMessage::Drain)
                {
                    warn!("the listener could not be drained ({e}); continuing the shutdown");
                }
                let _ = state
                    .infrastructure
                    .cast(InfrastructureSupervisorMessage::ProceedWithShutdown);
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let (cell, why) = match &event {
            SupervisionEvent::ActorTerminated(cell, _, reason) => (
                cell,
                reason
                    .clone()
                    .unwrap_or_else(|| "no reason given".to_string()),
            ),
            SupervisionEvent::ActorFailed(cell, error) => (cell, error.to_string()),
            _ => return Ok(()),
        };
        let child = child_of(
            cell.get_id(),
            state.infrastructure.get_id(),
            state.listener_id,
        );
        match on_child_ended(child, state.shutting_down, &why) {
            Ending::Stop => {
                info!("infrastructure shutdown complete ({why}), stopping");
                myself.stop(Some("the shutdown is complete".to_string()));
            }
            Ending::BeginShutdown(reason) => {
                error!("{reason}");
                let _ = state
                    .infrastructure
                    .cast(InfrastructureSupervisorMessage::BeginShutdown { reason });
            }
            Ending::Nothing => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listener_ending_outside_a_shutdown_begins_one_and_one_the_drain_asked_for_does_not() {
        match on_child_ended(Child::Listener, false, "the listener panicked: boom") {
            Ending::BeginShutdown(reason) => {
                assert!(reason.contains("the listener panicked: boom"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            on_child_ended(Child::Listener, true, "drained"),
            Ending::Nothing
        );
    }

    #[test]
    fn the_infrastructure_ending_stops_the_supervisor_in_or_out_of_a_shutdown() {
        for shutting_down in [false, true] {
            assert_eq!(
                on_child_ended(Child::Infrastructure, shutting_down, "stopped"),
                Ending::Stop
            );
        }
    }

    #[test]
    fn a_child_is_known_by_its_id() {
        use ractor::ActorId::Local;
        assert_eq!(
            child_of(Local(1), Local(1), Some(Local(2))),
            Child::Infrastructure
        );
        assert_eq!(
            child_of(Local(2), Local(1), Some(Local(2))),
            Child::Listener
        );
        assert_eq!(child_of(Local(3), Local(1), Some(Local(2))), Child::Other);
        assert_eq!(
            child_of(Local(2), Local(1), None),
            Child::Other,
            "before the listener exists"
        );
    }

    #[test]
    fn any_other_child_ending_changes_nothing() {
        for shutting_down in [false, true] {
            assert_eq!(
                on_child_ended(Child::Other, shutting_down, "x"),
                Ending::Nothing
            );
        }
    }
}
