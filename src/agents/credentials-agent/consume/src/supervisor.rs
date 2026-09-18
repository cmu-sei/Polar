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
use crate::certificate::CertificateHandler;
use crate::certificate_actor::{CertificateActor, CertificateActorArgs};
use crate::escrow_key_actor::{EscrowKeyActor, EscrowKeyActorArgs};
use crate::store_secret::StoreSecretHandler;
use crate::store_secret_actor::{StoreSecretActor, StoreSecretActorArgs, StoreSecretDispatch};
use crate::retrieve_secret::RetrieveSecretHandler;
use crate::retrieve_secret_actor::{RetrieveSecretActor, RetrieveSecretActorArgs, RetrieveSecretDispatch};
use crate::agent_escrow_keypair_bootstrap::AgentEscrowKeypairBootstrapHandler;
use crate::agent_escrow_keypair_bootstrap_actor::{
    AgentEscrowKeypairBootstrapActor, AgentEscrowKeypairBootstrapActorArgs,
    AgentEscrowKeypairBootstrapDispatch,
};
use crate::decrypt_on_behalf_of::DecryptOnBehalfOfHandler;
use crate::decrypt_on_behalf_of_actor::{
    DecryptOnBehalfOfActor, DecryptOnBehalfOfActorArgs, DecryptOnBehalfOfDispatch,
};
use credentials_agent_common::decrypt_on_behalf_of::DecryptOnBehalfOfRequest;
use credentials_agent_common::CREDENTIALS_DECRYPT_ON_BEHALF_OF_REQUESTS_TOPIC;
use credentials_agent_common::CREDENTIALS_CERTIFICATE_REQUESTS_TOPIC;
use credentials_agent_common::CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC;
use credentials_agent_common::CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC;
use credentials_agent_common::certificate::CertificateRequest;
use credentials_agent_common::escrow_key::EscrowKeyRequest;
use credentials_agent_common::secret::StoreSecretRequest;
use credentials_agent_common::retrieve_secret::RetrieveSecretRequest;
use credentials_agent_common::CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC;
use credentials_agent_common::agent_escrow_keypair_bootstrap::RetrieveAgentEscrowKeypairRequest;
use credentials_agent_common::CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC;
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
use crate::agent_escrow_issuance::AgentEscrowIssuanceHandler;
use crate::agent_escrow_issuance_actor::{AgentEscrowIssuanceActor, AgentEscrowIssuanceActorArgs};
use crate::agent_escrow_key::AgentEscrowKeyHandler;
use crate::agent_escrow_key_actor::{AgentEscrowKeyActor, AgentEscrowKeyActorArgs};
use cert_client::handshake::HandshakeClient;
use credentials_agent_common::CREDENTIALS_ISSUANCE_REQUESTS_TOPIC;
use credentials_agent_common::CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC;
use credentials_agent_common::CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC;
use credentials_agent_common::issuance::IssuanceRequest;
use credentials_agent_common::agent_escrow_issuance::AgentEscrowIssuanceRequest;
use credentials_agent_common::agent_escrow_key::AgentEscrowKeyRequest;
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
const CERTIFICATE_ACTOR_NAME: &str = "polar.credentials-agent.certificate";
const STORE_SECRET_ACTOR_NAME: &str = "polar.credentials-agent.store-secret";
const RETRIEVE_SECRET_ACTOR_NAME: &str = "polar.credentials-agent.retrieve-secret";
const AGENT_ESCROW_KEYPAIR_BOOTSTRAP_ACTOR_NAME: &str =
    "polar.credentials-agent.agent-escrow-keypair-bootstrap";
const AGENT_ESCROW_ISSUANCE_ACTOR_NAME: &str = "polar.credentials-agent.agent-escrow-issuance";
const AGENT_ESCROW_KEY_ACTOR_NAME: &str = "polar.credentials-agent.agent-escrow-key";
const DECRYPT_ON_BEHALF_OF_ACTOR_NAME: &str = "polar.credentials-agent.decrypt-on-behalf-of";

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

        if topic == CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC {
            match rkyv::from_bytes::<Envelope<AgentEscrowIssuanceRequest>, rkyv::rancor::Error>(&payload) {
                Ok(envelope) => {
                    if let Some(actor) = where_is(AGENT_ESCROW_ISSUANCE_ACTOR_NAME.to_string()) {
                        if let Err(e) = actor.send_message(envelope) {
                            warn!("Failed to forward agent escrow-issuance request to actor: {e}");
                        }
                    } else {
                        warn!("Agent escrow-issuance actor not yet registered, dropping request");
                    }
                }
                Err(e) => {
                    warn!("Failed to deserialize agent escrow-issuance request from topic {topic}: {e}");
                }
            }
            return;
        }

        if topic == CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC {
            match rkyv::from_bytes::<Envelope<AgentEscrowKeyRequest>, rkyv::rancor::Error>(&payload) {
                Ok(envelope) => {
                    if let Some(actor) = where_is(AGENT_ESCROW_KEY_ACTOR_NAME.to_string()) {
                        if let Err(e) = actor.send_message(envelope) {
                            warn!("Failed to forward agent escrow-key request to actor: {e}");
                        }
                    } else {
                        warn!("Agent escrow-key actor not yet registered, dropping request");
                    }
                }
                Err(e) => {
                    warn!("Failed to deserialize agent escrow-key request from topic {topic}: {e}");
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

        if topic == CREDENTIALS_CERTIFICATE_REQUESTS_TOPIC {
            match rkyv::from_bytes::<Envelope<CertificateRequest>, rkyv::rancor::Error>(&payload) {
                Ok(envelope) => {
                    if let Some(actor) = where_is(CERTIFICATE_ACTOR_NAME.to_string()) {
                        if let Err(e) = actor.send_message(envelope) {
                            warn!("Failed to forward certificate request to actor: {e}");
                        }
                    } else {
                        warn!("Certificate actor not yet registered, dropping request");
                    }
                }
                Err(e) => {
                    warn!("Failed to deserialize certificate request from topic {topic}: {e}");
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

impl ConsumerSupervisor {
    /// Not part of the shared Supervisor trait -- see its own call
    /// site's comment for why. Deserializes and forwards to
    /// StoreSecretActor, bundled with the broker-verified
    /// published_by this dispatch path exists specifically to carry.
    fn dispatch_store_secret_request(payload: Vec<u8>, published_by: Option<String>) {
        match rkyv::from_bytes::<Envelope<StoreSecretRequest>, rkyv::rancor::Error>(&payload) {
            Ok(envelope) => {
                if let Some(actor) = where_is(STORE_SECRET_ACTOR_NAME.to_string()) {
                    if let Err(e) = actor.send_message(StoreSecretDispatch {
                        envelope,
                        published_by,
                    }) {
                        warn!("Failed to forward store-secret request to actor: {e}");
                    }
                } else {
                    warn!("Store-secret actor not yet registered, dropping request");
                }
            }
            Err(e) => {
                warn!("Failed to deserialize store-secret request: {e}");
            }
        }
    }

    /// Mirrors dispatch_store_secret_request's own reasoning exactly
    /// -- see its own doc comment. Deserializes and forwards to
    /// RetrieveSecretActor, bundled with the broker-verified
    /// published_by this dispatch path exists specifically to carry.
    fn dispatch_retrieve_secret_request(payload: Vec<u8>, published_by: Option<String>) {
        match rkyv::from_bytes::<Envelope<RetrieveSecretRequest>, rkyv::rancor::Error>(&payload) {
            Ok(envelope) => {
                if let Some(actor) = where_is(RETRIEVE_SECRET_ACTOR_NAME.to_string()) {
                    if let Err(e) = actor.send_message(RetrieveSecretDispatch {
                        envelope,
                        published_by,
                    }) {
                        warn!("Failed to forward retrieve-secret request to actor: {e}");
                    }
                } else {
                    warn!("Retrieve-secret actor not yet registered, dropping request");
                }
            }
            Err(e) => {
                warn!("Failed to deserialize retrieve-secret request: {e}");
            }
        }
    }

    /// Mirrors dispatch_store_secret_request's own reasoning exactly
    /// -- see its own doc comment. Deserializes and forwards to
    /// AgentEscrowKeypairBootstrapActor, bundled with the
    /// broker-verified published_by this dispatch path exists
    /// specifically to carry.
    fn dispatch_agent_escrow_keypair_bootstrap_request(payload: Vec<u8>, published_by: Option<String>) {
        match rkyv::from_bytes::<Envelope<RetrieveAgentEscrowKeypairRequest>, rkyv::rancor::Error>(
            &payload,
        ) {
            Ok(envelope) => {
                if let Some(actor) = where_is(AGENT_ESCROW_KEYPAIR_BOOTSTRAP_ACTOR_NAME.to_string()) {
                    if let Err(e) = actor.send_message(AgentEscrowKeypairBootstrapDispatch {
                        envelope,
                        published_by,
                    }) {
                        warn!("Failed to forward agent escrow-keypair bootstrap request to actor: {e}");
                    }
                } else {
                    warn!("Agent escrow-keypair bootstrap actor not yet registered, dropping request");
                }
            }
            Err(e) => {
                warn!("Failed to deserialize agent escrow-keypair bootstrap request: {e}");
            }
        }
    }

    /// Mirrors dispatch_retrieve_secret_request's own reasoning
    /// exactly -- see its own doc comment. Deserializes and forwards
    /// to DecryptOnBehalfOfActor, bundled with the broker-verified
    /// published_by this dispatch path exists specifically to carry
    /// -- the requesting identity PolicyActor's own resolution is
    /// derived from, never self-reported.
    fn dispatch_decrypt_on_behalf_of_request(payload: Vec<u8>, published_by: Option<String>) {
        match rkyv::from_bytes::<Envelope<DecryptOnBehalfOfRequest>, rkyv::rancor::Error>(&payload) {
            Ok(envelope) => {
                if let Some(actor) = where_is(DECRYPT_ON_BEHALF_OF_ACTOR_NAME.to_string()) {
                    if let Err(e) = actor.send_message(DecryptOnBehalfOfDispatch {
                        envelope,
                        published_by,
                    }) {
                        warn!("Failed to forward decrypt-on-behalf-of request to actor: {e}");
                    }
                } else {
                    warn!("Decrypt-on-behalf-of actor not yet registered, dropping request");
                }
            }
            Err(e) => {
                warn!("Failed to deserialize decrypt-on-behalf-of request: {e}");
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

        let agent_escrow_issuance_handler = AgentEscrowIssuanceHandler {
            ledger: ledger.clone(),
            cert_issuer: HandshakeClient::new(cert_issuer_url.clone()),
            known_kubernetes_issuers: configured_kubernetes_issuers(),
            escrow_keypair: escrow_keypair.clone(),
        };
        let (_agent_escrow_issuance_actor, _) = AgentEscrowIssuanceActor::spawn_linked(
            Some(AGENT_ESCROW_ISSUANCE_ACTOR_NAME.to_string()),
            AgentEscrowIssuanceActor,
            AgentEscrowIssuanceActorArgs {
                handler: agent_escrow_issuance_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let agent_escrow_key_handler = AgentEscrowKeyHandler {
            ledger: ledger.clone(),
        };
        let (_agent_escrow_key_actor, _) = AgentEscrowKeyActor::spawn_linked(
            Some(AGENT_ESCROW_KEY_ACTOR_NAME.to_string()),
            AgentEscrowKeyActor,
            AgentEscrowKeyActorArgs {
                handler: agent_escrow_key_handler,
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

        let certificate_handler = CertificateHandler {
            ledger: ledger.clone(),
        };
        let (_certificate_actor, _) = CertificateActor::spawn_linked(
            Some(CERTIFICATE_ACTOR_NAME.to_string()),
            CertificateActor,
            CertificateActorArgs {
                handler: certificate_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let store_secret_handler = StoreSecretHandler {
            escrow_keypair: escrow_keypair.clone(),
            ledger: ledger.clone(),
        };
        let (_store_secret_actor, _) = StoreSecretActor::spawn_linked(
            Some(STORE_SECRET_ACTOR_NAME.to_string()),
            StoreSecretActor,
            StoreSecretActorArgs {
                handler: store_secret_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let retrieve_secret_handler = RetrieveSecretHandler {
            escrow_keypair: escrow_keypair.clone(),
            ledger: ledger.clone(),
        };
        let (_retrieve_secret_actor, _) = RetrieveSecretActor::spawn_linked(
            Some(RETRIEVE_SECRET_ACTOR_NAME.to_string()),
            RetrieveSecretActor,
            RetrieveSecretActorArgs {
                handler: retrieve_secret_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let agent_escrow_keypair_bootstrap_handler = AgentEscrowKeypairBootstrapHandler {
            escrow_keypair: escrow_keypair.clone(),
            ledger: ledger.clone(),
        };
        let (_agent_escrow_keypair_bootstrap_actor, _) = AgentEscrowKeypairBootstrapActor::spawn_linked(
            Some(AGENT_ESCROW_KEYPAIR_BOOTSTRAP_ACTOR_NAME.to_string()),
            AgentEscrowKeypairBootstrapActor,
            AgentEscrowKeypairBootstrapActorArgs {
                handler: agent_escrow_keypair_bootstrap_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        // PermissionsActor is the sole, real, authoritative source of
        // what permissions an identity actually holds -- PolicyActor
        // must never itself resolve permissions, only consume this
        // actor's own, real resolutions. See polar-permissions's own
        // crate documentation for the complete, real reasoning.
        let (permissions_actor, _) = polar_permissions::PermissionsActor::spawn_linked(
            None,
            polar_permissions::PermissionsActor,
            (),
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        // PolicyActor never exists without a real PermissionsActor --
        // construction requires one, structurally, not optionally.
        // See polar-policy's own crate documentation for the
        // complete, real reasoning behind this coupling.
        let (policy_actor, _) = polar_policy::PolicyActor::spawn_linked(
            None,
            polar_policy::PolicyActor,
            polar_policy::PolicyActorArgs { permissions_actor },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let decrypt_on_behalf_of_handler = DecryptOnBehalfOfHandler {
            escrow_keypair: escrow_keypair.clone(),
            ledger: ledger.clone(),
            policy_actor,
        };
        let (_decrypt_on_behalf_of_actor, _) = DecryptOnBehalfOfActor::spawn_linked(
            Some(DECRYPT_ON_BEHALF_OF_ACTOR_NAME.to_string()),
            DecryptOnBehalfOfActor,
            DecryptOnBehalfOfActorArgs {
                handler: decrypt_on_behalf_of_handler,
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
                delivery: cassini_client::DeliveryMode::Broadcast(events_output),
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
                            CREDENTIALS_CERTIFICATE_REQUESTS_TOPIC,
                            CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
                            CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC,
                            CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC,
                            CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC,
                            CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC,
                            CREDENTIALS_DECRYPT_ON_BEHALF_OF_REQUESTS_TOPIC,
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
                ClientEvent::MessagePublished {
                    topic,
                    payload,
                    published_by,
                    ..
                } => {
                    if state.draining {
                        warn!(
                            "CONSUMER_SUPERVISOR: draining -- logging message on topic \
                             '{topic}' ({} bytes) instead of dispatching; will be lost on \
                             exit",
                            payload.len()
                        );
                        // TODO: publish to dead-letter queue once Cassini supports one.
                    } else if topic == CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC {
                        // Not routed through the shared Supervisor
                        // trait's own deserialize_and_dispatch --
                        // that signature is cross-cutting (every
                        // agent implements it) and can't gain a
                        // published_by parameter just for this one,
                        // credentials-agent-specific need. This is
                        // the one dispatch path that genuinely
                        // requires it: stored_by must be
                        // broker-verified, never self-reported.
                        ConsumerSupervisor::dispatch_store_secret_request(payload, published_by);
                    } else if topic == CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC {
                        // Same reasoning as dispatch_store_secret_request
                        // above -- requested_by must be broker-verified,
                        // never self-reported, since it's what
                        // RetrieveSecretHandler derives the
                        // re-encryption target from.
                        ConsumerSupervisor::dispatch_retrieve_secret_request(payload, published_by);
                    } else if topic == CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC {
                        // Same reasoning as dispatch_store_secret_request
                        // above -- requested_by must be broker-verified,
                        // since it's what AgentEscrowKeypairBootstrapHandler
                        // derives the escrowed-secret lookup name from.
                        ConsumerSupervisor::dispatch_agent_escrow_keypair_bootstrap_request(
                            payload,
                            published_by,
                        );
                    } else if topic == CREDENTIALS_DECRYPT_ON_BEHALF_OF_REQUESTS_TOPIC {
                        // Same reasoning as dispatch_retrieve_secret_request
                        // above -- requested_by must be broker-verified,
                        // since it's the real, sole input PolicyActor's
                        // own CheckAuthorization derives the requester's
                        // resolved permissions from.
                        ConsumerSupervisor::dispatch_decrypt_on_behalf_of_request(
                            payload,
                            published_by,
                        );
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
    use credentials_agent_common::events::{GrantReason, RevocationReason};

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
            permissions: vec![cert_issuer_common::permission::Permission {
                scope: cert_issuer_common::permission::PermissionScope::ServiceToService,
                target: None,
            }],
            granted_by: "admin".to_string(),
            timestamp: 1000,
            reason: GrantReason::UserOnboarding,
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
            reason: RevocationReason::Compromised,
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
mod agent_escrow_wiring_tests {
    use super::*;
    use crate::agent_escrow_issuance::AgentEscrowIssuanceHandler;
    use crate::agent_escrow_issuance_actor::{AgentEscrowIssuanceActor, AgentEscrowIssuanceActorArgs};
    use crate::agent_escrow_key::AgentEscrowKeyHandler;
    use crate::agent_escrow_key_actor::{AgentEscrowKeyActor, AgentEscrowKeyActorArgs};
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use credentials_agent_common::agent_escrow_issuance::AgentEscrowIssuanceRequest;
    use credentials_agent_common::agent_escrow_key::{AgentEscrowKeyReply, AgentEscrowKeyRequest};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_token(iss: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"iss":"{iss}"}}"#));
        format!("{header}.{payload}.")
    }

    async fn check_agent_escrow_key(
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
        .unwrap()
        {
            ractor::rpc::CallResult::Success(result) => result,
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn full_pipeline_from_raw_bytes_through_dispatch_to_a_recorded_agent_escrow_issuance() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;
        // Mirrors issuance_wiring_tests's own
        // full_pipeline_from_raw_bytes_through_dispatch_to_a_recorded_issuance
        // exactly, proving the same real, end-to-end chain for the new
        // topic/actor pair specifically: raw bytes on
        // CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC ->
        // deserialize_and_dispatch -> AgentEscrowIssuanceActor's real
        // async handle() -> AgentEscrowIssuanceHandler's real logic ->
        // a genuinely recorded AgentEscrowIssuanceCompleted event.

        let cert_issuer = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "system:serviceaccount:polar:git-observer".to_string(),
                    instance_binding: Some("pod-uid-1".to_string()),
                    normalized_identity: None,
                },
            ))
            .mount(&cert_issuer)
            .await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::IssueResponse {
                    certificate_pem: "-----BEGIN CERTIFICATE-----\nfake-service\n-----END CERTIFICATE-----\n"
                        .to_string(),
                    ca_chain_pem: "-----BEGIN CERTIFICATE-----\nfake-ca\n-----END CERTIFICATE-----\n"
                        .to_string(),
                    serial_number: "agent-escrow-wiring-test-serial".to_string(),
                    session_id: "00000000-0000-0000-0000-000000000098".to_string(),
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

        let handler = AgentEscrowIssuanceHandler {
            ledger: ledger.clone(),
            cert_issuer: HandshakeClient::new(cert_issuer.uri()),
            known_kubernetes_issuers: vec!["https://kubernetes.default.svc.cluster.local".to_string()],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let (agent_escrow_issuance_actor, _) = AgentEscrowIssuanceActor::spawn(
            Some(AGENT_ESCROW_ISSUANCE_ACTOR_NAME.to_string()),
            AgentEscrowIssuanceActor,
            AgentEscrowIssuanceActorArgs { handler },
        )
        .await
        .unwrap();

        let request = AgentEscrowIssuanceRequest {
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
            CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC.to_string(),
            bytes,
        );

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let record = check_agent_escrow_key(&ledger, "system:serviceaccount:polar:git-observer").await;
        assert!(
            record.is_some(),
            "AgentEscrowIssuanceCompleted must have actually been recorded in the ledger by now"
        );
        assert_eq!(record.unwrap().serial, "agent-escrow-wiring-test-serial");

        agent_escrow_issuance_actor.stop_and_wait(None, None).await.unwrap();
        ledger.stop_and_wait(None, None).await.unwrap();
    }

    struct CapturingBrokerClient {
        captured: std::sync::Arc<tokio::sync::Mutex<Option<Vec<u8>>>>,
    }

    #[ractor::async_trait]
    impl ractor::Actor for CapturingBrokerClient {
        type Msg = TcpClientMessage;
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
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let TcpClientMessage::Publish { payload, .. } = message {
                *self.captured.lock().await = Some(payload);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn full_pipeline_from_raw_bytes_through_dispatch_answers_the_agent_escrow_key_query() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;
        // Same real-chain proof as the issuance test above, for the
        // agent escrow-key query specifically -- a genuinely separate
        // actor and topic pair. A real, capturing fake broker client
        // (registered under BROKER_CLIENT_NAME, the exact name
        // AgentEscrowKeyActor's own handle() looks up via where_is)
        // lets this test assert on the actual, real published reply
        // bytes, not merely that dispatch didn't panic -- this query
        // has no ledger side effect of its own to poll for the way
        // the issuance test above does.
        let captured = std::sync::Arc::new(tokio::sync::Mutex::new(None));
        let (broker_client, _) = ractor::Actor::spawn(
            Some(BROKER_CLIENT_NAME.to_string()),
            CapturingBrokerClient {
                captured: captured.clone(),
            },
            (),
        )
        .await
        .unwrap();

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

        // Seed a real requester certificate.
        let certificate_pem = {
            let mut rng = rand::rng();
            let private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
            let private_key_pem =
                rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(&private_key, rsa::pkcs8::LineEnding::LF)
                    .unwrap();
            let key_pair = rcgen::KeyPair::from_pem(&private_key_pem).unwrap();
            let params = rcgen::CertificateParams::new(vec![
                "git-observer.polar.svc.cluster.local".to_string(),
            ])
            .unwrap();
            params.self_signed(&key_pair).unwrap().pem()
        };
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: credentials_agent_common::events::CredentialsEvent::AgentEscrowIssuanceCompleted {
                    identity: "git-observer".to_string(),
                    instance_binding: "pod-uid-1".to_string(),
                    serial: "agent-escrow-key-wiring-test-serial".to_string(),
                    timestamp: 2000,
                    certificate_pem: certificate_pem.clone(),
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        let handler = AgentEscrowKeyHandler {
            ledger: ledger.clone(),
        };
        let (agent_escrow_key_actor, _) = AgentEscrowKeyActor::spawn(
            Some(AGENT_ESCROW_KEY_ACTOR_NAME.to_string()),
            AgentEscrowKeyActor,
            AgentEscrowKeyActorArgs { handler },
        )
        .await
        .unwrap();

        let request = AgentEscrowKeyRequest {
            identity: "git-observer".to_string(),
        };
        let envelope = Envelope {
            correlation_id: "test-correlation-id-2".to_string(),
            payload: request,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
            .unwrap()
            .to_vec();

        ConsumerSupervisor::deserialize_and_dispatch(
            CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC.to_string(),
            bytes,
        );

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let captured_bytes = captured
            .lock()
            .await
            .clone()
            .expect("the actor's own reply must have actually been published");
        let reply_envelope = rkyv::from_bytes::<Envelope<AgentEscrowKeyReply>, rkyv::rancor::Error>(
            &captured_bytes,
        )
        .unwrap();
        assert_eq!(reply_envelope.correlation_id, "test-correlation-id-2");
        match reply_envelope.payload {
            AgentEscrowKeyReply::Found { public_key_pem } => {
                let expected =
                    cert_issuer_common::public_key::extract_public_key_pem(&certificate_pem).unwrap();
                assert_eq!(public_key_pem, expected);
            }
            other => panic!("expected Found, got {other:?}"),
        }

        agent_escrow_key_actor.stop_and_wait(None, None).await.unwrap();
        broker_client.stop_and_wait(None, None).await.unwrap();
        ledger.stop_and_wait(None, None).await.unwrap();
    }
}

#[cfg(test)]
mod retrieve_secret_wiring_tests {
    use super::*;
    use crate::retrieve_secret::RetrieveSecretHandler;
    use crate::retrieve_secret_actor::{RetrieveSecretActor, RetrieveSecretActorArgs};
    use credentials_agent_common::escrow_secret::EscrowedSecret;
    use credentials_agent_common::retrieve_secret::{RetrieveSecretReply, RetrieveSecretRequest};

    struct CapturingBrokerClient {
        captured: std::sync::Arc<tokio::sync::Mutex<Option<Vec<u8>>>>,
    }

    #[ractor::async_trait]
    impl ractor::Actor for CapturingBrokerClient {
        type Msg = TcpClientMessage;
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
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let TcpClientMessage::Publish { payload, .. } = message {
                *self.captured.lock().await = Some(payload);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn full_pipeline_from_raw_bytes_through_the_special_dispatch_path_to_a_real_reply() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;
        // Proves the actual thing dispatch_store_secret_request's own
        // doc comment named as the real reason this dispatch path
        // exists outside the shared Supervisor trait: published_by
        // genuinely reaches the handler, derived from this call's own
        // argument, not from anything in the serialized request
        // bytes -- the same real, end-to-end proof
        // agent_escrow_wiring_tests already established for the
        // ordinary-path actors, applied here to the one, special
        // dispatch path this crate has that they don't use.

        let captured = std::sync::Arc::new(tokio::sync::Mutex::new(None));
        let (broker_client, _) = ractor::Actor::spawn(
            Some(BROKER_CLIENT_NAME.to_string()),
            CapturingBrokerClient {
                captured: captured.clone(),
            },
            (),
        )
        .await
        .unwrap();

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

        // Seed a real requester certificate.
        let mut rng = rand::rng();
        let requester_private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let requester_private_key_pem = rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(
            &requester_private_key,
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap();
        let requester_key_pair = rcgen::KeyPair::from_pem(&requester_private_key_pem).unwrap();
        let requester_params = rcgen::CertificateParams::new(vec![
            "requester.polar.svc.cluster.local".to_string(),
        ])
        .unwrap();
        let requester_certificate_pem =
            requester_params.self_signed(&requester_key_pair).unwrap().pem();
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: credentials_agent_common::events::CredentialsEvent::IssuanceCompleted {
                    identity: "requester".to_string(),
                    instance_binding: "pod-uid-1".to_string(),
                    serial: "requester-serial-1".to_string(),
                    timestamp: 1000,
                    certificate_pem: requester_certificate_pem,
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        // Seed a real, stored secret.
        let escrow_keypair = crate::escrow::tests::test_keypair();
        let original_plaintext = b"a real secret via the real dispatch path";
        let stored_secret =
            EscrowedSecret::encrypt(original_plaintext, escrow_keypair.public_key_pem()).unwrap();
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: credentials_agent_common::events::CredentialsEvent::SecretStored {
                    name: "gitlab-token".to_string(),
                    secret: stored_secret,
                    stored_by: "some-storing-agent".to_string(),
                    timestamp: 1000,
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        let handler = RetrieveSecretHandler {
            escrow_keypair,
            ledger: ledger.clone(),
        };
        let (retrieve_secret_actor, _) = RetrieveSecretActor::spawn(
            Some(RETRIEVE_SECRET_ACTOR_NAME.to_string()),
            RetrieveSecretActor,
            RetrieveSecretActorArgs { handler },
        )
        .await
        .unwrap();

        let request = RetrieveSecretRequest {
            name: "gitlab-token".to_string(),
        };
        let envelope = Envelope {
            correlation_id: "test-correlation-id-retrieve".to_string(),
            payload: request,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
            .unwrap()
            .to_vec();

        // The actual, real, private dispatch function this test
        // exists to exercise -- not deserialize_and_dispatch, since
        // this specific topic is deliberately routed elsewhere (see
        // ConsumerSupervisor's own ClientEvent::MessagePublished
        // handler and dispatch_retrieve_secret_request's own doc
        // comment for why).
        ConsumerSupervisor::dispatch_retrieve_secret_request(
            bytes,
            Some("requester".to_string()),
        );

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let captured_bytes = captured
            .lock()
            .await
            .clone()
            .expect("the actor's own reply must have actually been published");
        let reply_envelope = rkyv::from_bytes::<Envelope<RetrieveSecretReply>, rkyv::rancor::Error>(
            &captured_bytes,
        )
        .unwrap();
        assert_eq!(reply_envelope.correlation_id, "test-correlation-id-retrieve");
        match reply_envelope.payload {
            RetrieveSecretReply::Found { secret } => {
                let decrypted = secret.decrypt(&requester_private_key).unwrap();
                assert_eq!(&*decrypted, original_plaintext);
            }
            other => panic!("expected Found, got {other:?}"),
        }

        retrieve_secret_actor.stop_and_wait(None, None).await.unwrap();
        broker_client.stop_and_wait(None, None).await.unwrap();
        ledger.stop_and_wait(None, None).await.unwrap();
    }
}

#[cfg(test)]
mod agent_escrow_keypair_bootstrap_wiring_tests {
    use super::*;
    use crate::agent_escrow_keypair_bootstrap::AgentEscrowKeypairBootstrapHandler;
    use crate::agent_escrow_keypair_bootstrap_actor::{
        AgentEscrowKeypairBootstrapActor, AgentEscrowKeypairBootstrapActorArgs,
    };
    use credentials_agent_common::agent_escrow_keypair_bootstrap::{
        RetrieveAgentEscrowKeypairReply, RetrieveAgentEscrowKeypairRequest,
        agent_escrow_keypair_secret_name,
    };
    use credentials_agent_common::escrow_secret::EscrowedSecret;
    use x25519_dalek::{EphemeralSecret, PublicKey};

    struct CapturingBrokerClient {
        captured: std::sync::Arc<tokio::sync::Mutex<Option<Vec<u8>>>>,
    }

    #[ractor::async_trait]
    impl ractor::Actor for CapturingBrokerClient {
        type Msg = TcpClientMessage;
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
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let TcpClientMessage::Publish { payload, .. } = message {
                *self.captured.lock().await = Some(payload);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn full_pipeline_from_raw_bytes_through_the_special_dispatch_path_to_a_real_reply() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;
        // Mirrors retrieve_secret_wiring_tests's own test of the
        // identical shape -- proves published_by genuinely reaches
        // this handler too, via this crate's second dispatch path
        // outside the shared Supervisor trait.

        let captured = std::sync::Arc::new(tokio::sync::Mutex::new(None));
        let (broker_client, _) = ractor::Actor::spawn(
            Some(BROKER_CLIENT_NAME.to_string()),
            CapturingBrokerClient {
                captured: captured.clone(),
            },
            (),
        )
        .await
        .unwrap();

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

        // Seed a real, stored "escrow keypair" secret.
        let escrow_keypair = crate::escrow::tests::test_keypair();
        let original_plaintext = b"a real escrowed keypair, via the real dispatch path";
        let stored_secret =
            EscrowedSecret::encrypt(original_plaintext, escrow_keypair.public_key_pem()).unwrap();
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: credentials_agent_common::events::CredentialsEvent::SecretStored {
                    name: agent_escrow_keypair_secret_name("git-observer"),
                    secret: stored_secret,
                    stored_by: "git-observer".to_string(),
                    timestamp: 1000,
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        let handler = AgentEscrowKeypairBootstrapHandler {
            escrow_keypair,
            ledger: ledger.clone(),
        };
        let (agent_escrow_keypair_bootstrap_actor, _) = AgentEscrowKeypairBootstrapActor::spawn(
            Some(AGENT_ESCROW_KEYPAIR_BOOTSTRAP_ACTOR_NAME.to_string()),
            AgentEscrowKeypairBootstrapActor,
            AgentEscrowKeypairBootstrapActorArgs { handler },
        )
        .await
        .unwrap();

        let requester_secret = EphemeralSecret::random();
        let requester_public_key = PublicKey::from(&requester_secret).to_bytes();

        let request = RetrieveAgentEscrowKeypairRequest {
            ephemeral_public_key: requester_public_key,
        };
        let envelope = Envelope {
            correlation_id: "test-correlation-id-bootstrap".to_string(),
            payload: request,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
            .unwrap()
            .to_vec();

        // The actual, real, private dispatch function this test
        // exists to exercise.
        ConsumerSupervisor::dispatch_agent_escrow_keypair_bootstrap_request(
            bytes,
            Some("git-observer".to_string()),
        );

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let captured_bytes = captured
            .lock()
            .await
            .clone()
            .expect("the actor's own reply must have actually been published");
        let reply_envelope = rkyv::from_bytes::<Envelope<RetrieveAgentEscrowKeypairReply>, rkyv::rancor::Error>(&captured_bytes)
            .unwrap();
        assert_eq!(reply_envelope.correlation_id, "test-correlation-id-bootstrap");
        match reply_envelope.payload {
            RetrieveAgentEscrowKeypairReply::Found { payload } => {
                let decrypted = payload.decrypt(requester_secret).unwrap();
                assert_eq!(decrypted.as_slice(), original_plaintext);
            }
            other => panic!("expected Found, got {other:?}"),
        }

        agent_escrow_keypair_bootstrap_actor
            .stop_and_wait(None, None)
            .await
            .unwrap();
        broker_client.stop_and_wait(None, None).await.unwrap();
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

#[cfg(test)]
mod decrypt_on_behalf_of_wiring_tests {
    use super::*;
    use crate::decrypt_on_behalf_of::DecryptOnBehalfOfHandler;
    use crate::decrypt_on_behalf_of_actor::{DecryptOnBehalfOfActor, DecryptOnBehalfOfActorArgs};
    use credentials_agent_common::decrypt_on_behalf_of::{
        DecryptOnBehalfOfReply, DecryptOnBehalfOfRequest,
    };
    use credentials_agent_common::escrow_secret::EscrowedSecret;

    struct CapturingBrokerClient {
        captured: std::sync::Arc<tokio::sync::Mutex<Option<Vec<u8>>>>,
    }

    #[ractor::async_trait]
    impl ractor::Actor for CapturingBrokerClient {
        type Msg = TcpClientMessage;
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
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let TcpClientMessage::Publish { payload, .. } = message {
                *self.captured.lock().await = Some(payload);
            }
            Ok(())
        }
    }

    async fn spawn_policy_actor_permitting(
        identity: &str,
        resource_id: &str,
    ) -> ActorRef<polar_policy::PolicyMessage> {
        let (permissions_actor, _) =
            ractor::Actor::spawn(None, polar_permissions::PermissionsActor, ())
                .await
                .unwrap();
        let (policy_actor, _) = ractor::Actor::spawn(
            None,
            polar_policy::PolicyActor,
            polar_policy::PolicyActorArgs { permissions_actor },
        )
        .await
        .unwrap();

        let policy = format!(
            r#"
            permit(
                principal == Identity::"{identity}",
                action == Action::"decrypt",
                resource == {resource_id}
            );
            "#
        );
        let result = ractor::call!(policy_actor, |reply| polar_policy::PolicyMessage::UpdatePolicy {
            policy_source: policy,
            reply,
        })
        .unwrap();
        result.unwrap();

        policy_actor
    }

    #[tokio::test]
    async fn full_pipeline_from_raw_bytes_through_the_special_dispatch_path_to_a_real_allowed_reply() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;
        // Mirrors retrieve_secret_wiring_tests's own test of the
        // identical shape -- proves published_by genuinely reaches
        // this handler too, via this crate's real, new
        // dispatch_decrypt_on_behalf_of_request path, and that the
        // real, complete flow (certificate lookup, live PolicyActor
        // query, decrypt, re-encrypt) actually works end-to-end
        // through the real dispatch function, not merely the
        // handler in isolation (already proven directly in
        // decrypt_on_behalf_of.rs's own test module).

        let captured = std::sync::Arc::new(tokio::sync::Mutex::new(None));
        let (broker_client, _) = ractor::Actor::spawn(
            Some(BROKER_CLIENT_NAME.to_string()),
            CapturingBrokerClient {
                captured: captured.clone(),
            },
            (),
        )
        .await
        .unwrap();

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

        // Seed a real requester certificate.
        let mut rng = rand::rng();
        let requester_private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let requester_private_key_pem = rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(
            &requester_private_key,
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap();
        let requester_key_pair = rcgen::KeyPair::from_pem(&requester_private_key_pem).unwrap();
        let requester_params = rcgen::CertificateParams::new(vec![
            "requester.polar.svc.cluster.local".to_string(),
        ])
        .unwrap();
        let requester_certificate_pem =
            requester_params.self_signed(&requester_key_pair).unwrap().pem();
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: credentials_agent_common::events::CredentialsEvent::IssuanceCompleted {
                    identity: "requester".to_string(),
                    instance_binding: "pod-uid-1".to_string(),
                    serial: "requester-serial-1".to_string(),
                    timestamp: 1000,
                    certificate_pem: requester_certificate_pem,
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        let resource_id = r#"KubeSecret::"namespace/pod/ENV_VAR""#;
        let policy_actor = spawn_policy_actor_permitting("requester", resource_id).await;

        let escrow_keypair = crate::escrow::tests::test_keypair();
        let original_plaintext = b"a real, plaintext env var value, via the real dispatch path";
        let ciphertext =
            EscrowedSecret::encrypt(original_plaintext, escrow_keypair.public_key_pem()).unwrap();

        let handler = DecryptOnBehalfOfHandler {
            escrow_keypair,
            ledger: ledger.clone(),
            policy_actor,
        };
        let (decrypt_on_behalf_of_actor, _) = DecryptOnBehalfOfActor::spawn(
            Some(DECRYPT_ON_BEHALF_OF_ACTOR_NAME.to_string()),
            DecryptOnBehalfOfActor,
            DecryptOnBehalfOfActorArgs { handler },
        )
        .await
        .unwrap();

        let request = DecryptOnBehalfOfRequest {
            ciphertext,
            resource_id: resource_id.to_string(),
        };
        let envelope = Envelope {
            correlation_id: "test-correlation-id-decrypt-on-behalf-of".to_string(),
            payload: request,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
            .unwrap()
            .to_vec();

        // The actual, real, private dispatch function this test
        // exists to exercise.
        ConsumerSupervisor::dispatch_decrypt_on_behalf_of_request(
            bytes,
            Some("requester".to_string()),
        );

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let captured_bytes = captured
            .lock()
            .await
            .clone()
            .expect("the actor's own reply must have actually been published");
        let reply_envelope = rkyv::from_bytes::<Envelope<DecryptOnBehalfOfReply>, rkyv::rancor::Error>(
            &captured_bytes,
        )
        .unwrap();
        assert_eq!(
            reply_envelope.correlation_id,
            "test-correlation-id-decrypt-on-behalf-of"
        );
        match reply_envelope.payload {
            DecryptOnBehalfOfReply::Allowed { secret } => {
                let decrypted = secret.decrypt(&requester_private_key).unwrap();
                assert_eq!(&*decrypted, original_plaintext);
            }
            other => panic!("expected Allowed, got {other:?}"),
        }

        decrypt_on_behalf_of_actor.stop_and_wait(None, None).await.unwrap();
        broker_client.stop_and_wait(None, None).await.unwrap();
        ledger.stop_and_wait(None, None).await.unwrap();
    }
}
