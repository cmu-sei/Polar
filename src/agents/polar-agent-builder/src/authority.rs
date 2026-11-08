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

//! The leaf's side of authority distribution (docs/architecture/authority-distribution.md): the actor that keeps an
//! agent's policy, groups and revoked serials current. The decision, with no actors in it, is
//! [`AuthorityCore`] (`credentials_agent_common::authority_leaf`), which the custody daemon keeps too.

pub use credentials_agent_common::authority_leaf::{AuthorityCore, Effect, Handled, Refusal};
use credentials_agent_common::authority_update::{AuthorityKind, AuthoritySnapshot};

// -- the actor ----------------------------------------------------------------------------------------

use crate::signed::Revocation;
use credentials_agent_common::authority_update::AuthorityUpdate as Update;
use polar_permissions::{PermissionsActor, PermissionsMessage};
use polar_policy::{PolicyActor, PolicyActorArgs, PolicyMessage};
use ractor::{Actor, ActorCell, ActorProcessingErr, ActorRef, OutputPort, RpcReplyPort};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

/// Sent when authority of a kind changed, for any actor that holds something derived from it (a session, a
/// cached decision) to drop at once. The counterpart of `ConfigChanged`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityChanged {
    pub kind: AuthorityKind,
    pub version: u64,
}

/// The agent wants the current authority from the credentials agent. The infrastructure turns it into a
/// request on the Cassini client as it is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorityAsk;

/// The certificate serials the credentials agent says are revoked. Signer verification consults it, so a
/// demotion by revocation is refused at this agent as soon as the update arrives.
#[derive(Clone, Default)]
pub struct RevokedSerials(Arc<RwLock<HashMap<String, RevokedFor>>>);

/// Why a certificate was revoked, as the signed revoked set says (`AuthorityBody::Revoked`): an ordinary end (superseded,
/// decommissioned, ...), or a compromise, which is what starts a rotation (docs/architecture/key-rotation.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokedFor {
    Ordinary,
    Compromise,
}

impl RevokedSerials {
    fn replace(&self, serials: Vec<String>, compromised: Vec<String>) {
        let compromised: HashSet<String> = compromised.into_iter().collect();
        *self
            .0
            .write()
            .expect("the revoked set is never held across a panic") = serials
            .into_iter()
            .map(|serial| {
                let why = if compromised.contains(&serial) {
                    RevokedFor::Compromise
                } else {
                    RevokedFor::Ordinary
                };
                (serial, why)
            })
            .collect();
    }

    /// Whether the certificate whose serial is `serial` (as `cert_issuer_common::serial` writes it) is revoked.
    pub fn contains(&self, serial: &str) -> bool {
        self.revoked_for(serial).is_some()
    }

    /// Why the certificate whose serial is `serial` was revoked, or none if it is not.
    pub fn revoked_for(&self, serial: &str) -> Option<RevokedFor> {
        self.0
            .read()
            .expect("the revoked set is never held across a panic")
            .get(serial)
            .copied()
    }
}

impl Revocation for RevokedSerials {
    fn is_revoked(&self, signer: &polar_signing::SignerInfo) -> bool {
        self.contains(&signer.serial)
    }
}

/// The two subscriptions every Cassini client agent has to the credentials agent's authority: a live update, and
/// a snapshot (asked for, or republished). Each is handed to the authority actor with who the broker says
/// published it; one that does not decode is dropped.
pub fn subscriptions(authority: &AuthorityHandle) -> Vec<crate::Subscription> {
    use credentials_agent_common::authority_update::{
        AUTHORITY_SNAPSHOT_REPLIES_TOPIC, AUTHORITY_UPDATES_TOPIC,
    };
    let for_updates = authority.clone();
    let for_snapshots = authority.clone();
    vec![
        crate::Subscription::new(
            AUTHORITY_UPDATES_TOPIC,
            move |incoming| match rkyv::from_bytes::<Update, rkyv::rancor::Error>(&incoming.payload)
            {
                Ok(update) => for_updates.update(update, incoming.published_by),
                Err(e) => warn!("a message on the authority updates topic is not an update: {e}"),
            },
        ),
        crate::Subscription::new(AUTHORITY_SNAPSHOT_REPLIES_TOPIC, move |incoming| {
            match rkyv::from_bytes::<AuthoritySnapshot, rkyv::rancor::Error>(&incoming.payload) {
                Ok(snapshot) => for_snapshots.snapshot(snapshot, incoming.published_by),
                Err(e) => warn!("a message on the authority snapshot topic is not a snapshot: {e}"),
            }
        }),
    ]
}

/// The least time between two requests for the current authority: an agent that keeps seeing updates it cannot
/// check, or a jump it keeps seeing, asks once, not once per message.
const ASK_INTERVAL: Duration = Duration::from_secs(5);
/// How often an agent that has no key yet asks again.
const TICK: Duration = Duration::from_secs(30);

pub enum AuthorityMessage {
    Update {
        update: Update,
        published_by: Option<String>,
    },
    Snapshot {
        snapshot: AuthoritySnapshot,
        published_by: Option<String>,
    },
    /// The Cassini client registered (again): ask for the current authority.
    Registered,
    /// The broker refused what this agent's own gate let through: the authority it holds may be behind the
    /// broker's, so ask for the current one.
    Behind,
    /// Time to ask again if there is still no key.
    Tick,
    Versions(RpcReplyPort<HashMap<AuthorityKind, u64>>),
}

pub struct AuthorityActor;

pub struct AuthorityArgs {
    pub publisher: String,
    pub permissions: ActorRef<PermissionsMessage>,
    pub policy: ActorRef<PolicyMessage>,
    pub revoked: RevokedSerials,
    pub changes: Arc<OutputPort<AuthorityChanged>>,
    pub asks: Arc<OutputPort<AuthorityAsk>>,
}

pub struct AuthorityActorState {
    core: AuthorityCore,
    args: AuthorityArgs,
    last_ask: Option<Instant>,
}

impl AuthorityActorState {
    fn ask(&mut self) {
        let now = Instant::now();
        if self
            .last_ask
            .is_some_and(|at| now.duration_since(at) < ASK_INTERVAL)
        {
            debug!("not asking for the authority again so soon");
            return;
        }
        self.last_ask = Some(now);
        self.args.asks.send(AuthorityAsk);
    }

    async fn carry_out(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::SetPolicy(source) => {
                    let result =
                        ractor::call!(self.args.policy, |reply| PolicyMessage::UpdatePolicy {
                            policy_source: source,
                            reply,
                        });
                    match result {
                        Ok(Ok(())) => {}
                        // The credentials agent validates a policy before it signs it, so this is its fault
                        // or a version skew; the rules this agent had stay in force.
                        Ok(Err(e)) => {
                            error!("the policy the credentials agent sent was not loaded: {e}")
                        }
                        Err(e) => error!("the policy actor did not answer: {e}"),
                    }
                }
                Effect::SetGroups(memberships, hierarchy) => {
                    let memberships: HashMap<String, Vec<String>> =
                        memberships.into_iter().collect();
                    if let Err(e) = ractor::call!(self.args.permissions, |reply| {
                        PermissionsMessage::UpdateGroups {
                            memberships,
                            hierarchy,
                            reply,
                        }
                    }) {
                        error!("the permissions actor did not take the groups: {e}");
                    }
                }
                Effect::SetRevoked {
                    serials,
                    compromised,
                } => self.args.revoked.replace(serials, compromised),
                Effect::Changed { kind, version } => {
                    info!(?kind, version, "authority changed");
                    self.args.changes.send(AuthorityChanged { kind, version });
                }
                Effect::AskForSnapshot => self.ask(),
            }
        }
    }

    async fn handled(&mut self, result: Result<Handled, Refusal>) {
        match result {
            Ok(handled) => {
                for refused in &handled.refused {
                    warn!("an authority update in a snapshot was refused: {refused}");
                }
                self.carry_out(handled.effects).await;
            }
            Err(Refusal::NoKeyYet) => self.ask(),
            Err(Refusal::WrongPublisher { got }) => {
                debug!(
                    ?got,
                    "authority from someone other than the credentials agent, ignored"
                );
            }
            Err(refusal) => warn!(?refusal, "authority refused"),
        }
    }
}

#[ractor::async_trait]
impl Actor for AuthorityActor {
    type Msg = AuthorityMessage;
    type State = AuthorityActorState;
    /// What it is wired to, and the CA the agent pins, under which the credentials agent's key is certified.
    type Arguments = (AuthorityArgs, polar_signing::Trust);

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        (args, trust): (AuthorityArgs, polar_signing::Trust),
    ) -> Result<Self::State, ActorProcessingErr> {
        // While there is no key it asks again from time to time; with one the tick does nothing.
        drop(myself.send_interval(TICK, || AuthorityMessage::Tick));
        Ok(AuthorityActorState {
            core: AuthorityCore::new(args.publisher.clone(), trust),
            args,
            last_ask: None,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            AuthorityMessage::Update {
                update,
                published_by,
            } => {
                let result = state.core.on_update(&update, published_by.as_deref());
                state.handled(result).await;
            }
            AuthorityMessage::Snapshot {
                snapshot,
                published_by,
            } => {
                let result = state
                    .core
                    .on_snapshot(&snapshot, published_by.as_deref(), unix_now());
                state.handled(result).await;
            }
            AuthorityMessage::Registered | AuthorityMessage::Behind => state.ask(),
            AuthorityMessage::Tick => {
                if !state.core.has_key() {
                    state.ask();
                }
            }
            AuthorityMessage::Versions(reply) => {
                let _ = reply.send(state.core.state().versions());
            }
        }
        Ok(())
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The registry name of an agent's policy actor, the way the Cassini client is "<service>.tcp".
pub fn policy_actor_name(service: &str) -> String {
    format!("{service}.policy")
}

/// The registry name of an agent's permissions actor.
pub fn permissions_actor_name(service: &str) -> String {
    format!("{service}.permissions")
}

/// How code that enforces policy reaches the agent's policy actor: by registry name, on every decision, so it
/// can be built before the infrastructure exists and follows an actor that is respawned. Until the actor is
/// there it decides nothing, and an asker that gets an error must not grant: it fails closed.
#[derive(Clone, Debug)]
pub struct PolicyLookup {
    name: String,
}

impl PolicyLookup {
    /// The policy actor of the agent named `service`.
    pub fn for_service(service: &str) -> Self {
        Self::named(policy_actor_name(service))
    }

    /// The policy actor registered as `name`.
    pub fn named(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    pub fn actor(&self) -> Option<ActorRef<PolicyMessage>> {
        ractor::registry::where_is(self.name.clone()).map(ActorRef::from)
    }

    /// What policy says about `action` on `resource` for the requester. An error means no decision was
    /// reached (no policy actor yet, it did not answer, or it failed), never a decision: a caller that
    /// is told this must refuse. The same as [`PolicyLookup::decide`], with the error as text.
    pub async fn check(
        &self,
        requester_identity: &str,
        requester_certificate_pem: Option<String>,
        action: &str,
        resource: &str,
        facts: polar_policy::Facts,
    ) -> Result<polar_policy::Decision, String> {
        self.decide(
            requester_identity,
            requester_certificate_pem,
            action,
            resource,
            facts,
        )
        .await
        .map_err(|error| error.to_string())
    }

    /// What policy says, with the reason it said nothing told apart: [`LookupError::NoPolicyLoaded`] is a policy
    /// that is running and has been given no rules, which a caller with a floor to fall back to (a store, on a
    /// fresh cluster) can act on, and [`LookupError::Unavailable`] is no answer at all, which no caller may
    /// treat as a decision.
    pub async fn decide(
        &self,
        requester_identity: &str,
        requester_certificate_pem: Option<String>,
        action: &str,
        resource: &str,
        facts: polar_policy::Facts,
    ) -> Result<polar_policy::Decision, LookupError> {
        let Some(policy) = self.actor() else {
            return Err(LookupError::Unavailable(
                "the policy is not running yet, so nothing can be decided".to_string(),
            ));
        };
        match ractor::rpc::call(
            &policy,
            |reply| PolicyMessage::CheckAuthorization {
                requester_identity: requester_identity.to_string(),
                requester_certificate_pem,
                action: action.to_string(),
                resource: resource.to_string(),
                facts,
                reply,
            },
            None,
        )
        .await
        {
            Ok(ractor::rpc::CallResult::Success(Ok(decision))) => Ok(decision),
            Ok(ractor::rpc::CallResult::Success(Err(
                polar_policy::PolicyError::NoPolicyLoaded,
            ))) => Err(LookupError::NoPolicyLoaded),
            Ok(ractor::rpc::CallResult::Success(Err(e))) => {
                Err(LookupError::Unavailable(e.to_string()))
            }
            Ok(other) => Err(LookupError::Unavailable(format!(
                "unexpected CallResult: {other:?}"
            ))),
            Err(e) => Err(LookupError::Unavailable(e.to_string())),
        }
    }
}

/// Why policy gave no decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupError {
    /// The policy actor is running and has no rules.
    NoPolicyLoaded,
    /// There was no answer: no policy actor, it did not reply, or it failed.
    Unavailable(String),
}

impl std::fmt::Display for LookupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LookupError::NoPolicyLoaded => {
                write!(f, "{}", polar_policy::PolicyError::NoPolicyLoaded)
            }
            LookupError::Unavailable(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for LookupError {}

/// How the rest of an agent uses authority, without touching ractor: the actors it evaluates with, the
/// revoked serials, and the ports its own actors listen on.
#[derive(Clone)]
pub struct AuthorityHandle {
    actor: ActorRef<AuthorityMessage>,
    permissions: ActorRef<PermissionsMessage>,
    policy: ActorRef<PolicyMessage>,
    revoked: RevokedSerials,
    changes: Arc<OutputPort<AuthorityChanged>>,
    asks: Arc<OutputPort<AuthorityAsk>>,
}

impl AuthorityHandle {
    /// Starts the permissions actor, the policy actor over it, and the actor that keeps both current, all as
    /// children of `supervisor`. `publisher` is the identity the broker gives the credentials agent. The
    /// policy and permissions actors are registered under the names [`policy_actor_name`] and
    /// [`permissions_actor_name`] give for `service`, so code built before the infrastructure (handlers that
    /// enforce policy) finds them with a [`PolicyLookup`] when it needs them.
    pub async fn spawn(
        service: &str,
        publisher: impl Into<String>,
        trust: polar_signing::Trust,
        supervisor: ActorCell,
    ) -> Result<Self, ActorProcessingErr> {
        let (permissions, _) = Actor::spawn_linked(
            Some(permissions_actor_name(service)),
            PermissionsActor,
            (),
            supervisor.clone(),
        )
        .await?;
        let (policy, _) = Actor::spawn_linked(
            Some(policy_actor_name(service)),
            PolicyActor,
            PolicyActorArgs {
                permissions_actor: permissions.clone(),
            },
            supervisor.clone(),
        )
        .await?;
        let revoked = RevokedSerials::default();
        let changes = Arc::new(OutputPort::default());
        let asks = Arc::new(OutputPort::default());
        let (actor, _) = Actor::spawn_linked(
            None,
            AuthorityActor,
            (
                AuthorityArgs {
                    publisher: publisher.into(),
                    permissions: permissions.clone(),
                    policy: policy.clone(),
                    revoked: revoked.clone(),
                    changes: changes.clone(),
                    asks: asks.clone(),
                },
                trust,
            ),
            supervisor,
        )
        .await?;
        Ok(Self {
            actor,
            permissions,
            policy,
            revoked,
            changes,
            asks,
        })
    }

    pub fn update(&self, update: Update, published_by: Option<String>) {
        self.cast(AuthorityMessage::Update {
            update,
            published_by,
        });
    }

    pub fn snapshot(&self, snapshot: AuthoritySnapshot, published_by: Option<String>) {
        self.cast(AuthorityMessage::Snapshot {
            snapshot,
            published_by,
        });
    }

    /// The Cassini client registered: ask for the current authority.
    pub fn registered(&self) {
        self.cast(AuthorityMessage::Registered);
    }

    /// The broker refused what this agent's gate let through: ask for the current authority, as for an update that
    /// cannot be checked, no more than once in the interval.
    pub fn behind(&self) {
        self.cast(AuthorityMessage::Behind);
    }

    pub fn tick(&self) {
        self.cast(AuthorityMessage::Tick);
    }

    fn cast(&self, message: AuthorityMessage) {
        if let Err(e) = self.actor.cast(message) {
            warn!("the authority actor did not take a message: {e}");
        }
    }

    /// The version held of each kind.
    pub async fn versions(&self) -> Result<HashMap<AuthorityKind, u64>, String> {
        ractor::call!(self.actor, AuthorityMessage::Versions).map_err(|e| e.to_string())
    }

    pub fn policy(&self) -> ActorRef<PolicyMessage> {
        self.policy.clone()
    }

    pub fn permissions(&self) -> ActorRef<PermissionsMessage> {
        self.permissions.clone()
    }

    /// Signer verification consults this.
    pub fn revoked(&self) -> RevokedSerials {
        self.revoked.clone()
    }

    /// Where a change of authority is announced. Subscribe with a mapping into your own message type, as for
    /// `ConfigurationHandle::changes`.
    pub fn changes(&self) -> Arc<OutputPort<AuthorityChanged>> {
        self.changes.clone()
    }

    /// Where the agent says it wants the current authority.
    pub fn asks(&self) -> Arc<OutputPort<AuthorityAsk>> {
        self.asks.clone()
    }
}

#[cfg(test)]
mod actor_tests {
    use super::*;
    use credentials_agent_common::authority_update::{AuthorityBody, AuthorityPayload, sign};
    use ractor::{Actor, ActorRef};
    use rsa::RsaPrivateKey;
    use rsa::pkcs8::EncodePublicKey;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const CREDENTIALS: &str = "credentials-agent.polar.serviceaccount.cluster.local";

    fn key() -> RsaPrivateKey {
        use std::sync::OnceLock;
        static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
        KEY.get_or_init(|| RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap())
            .clone()
    }

    fn ca() -> &'static polar_signing::testing::Authority {
        static CA: std::sync::OnceLock<polar_signing::testing::Authority> =
            std::sync::OnceLock::new();
        CA.get_or_init(|| polar_signing::testing::root("Polar Internal CA"))
    }

    /// What the agent pins: the CA its tests' credentials agent is certified under.
    fn trust() -> polar_signing::Trust {
        polar_signing::Trust::from_pem(&[ca().pem.clone()]).unwrap()
    }

    fn snapshot(payloads: &[AuthorityPayload]) -> AuthoritySnapshot {
        let public_key_pem = key()
            .to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();
        AuthoritySnapshot {
            certificate_chain_pem: vec![polar_signing::testing::escrow_certificate_for(
                ca(),
                polar_signing::testing::authority_spec(CREDENTIALS),
                &public_key_pem,
            )],
            public_key_pem: key()
                .to_public_key()
                .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
                .unwrap(),
            updates: payloads.iter().map(|p| sign(p, &key()).unwrap()).collect(),
        }
    }

    fn payload(sequence: u64, version: u64, body: AuthorityBody) -> AuthorityPayload {
        AuthorityPayload {
            sequence,
            version,
            body,
            authored_by: None,
        }
    }

    fn policy(sequence: u64, version: u64, source: &str) -> AuthorityPayload {
        payload(
            sequence,
            version,
            AuthorityBody::Policy {
                source: source.into(),
            },
        )
    }

    /// Records what comes out of a port.
    struct Sink<T>(Arc<Mutex<Vec<T>>>);

    #[ractor::async_trait]
    impl<T: Send + Sync + 'static> Actor for Sink<T> {
        type Msg = T;
        type State = ();
        type Arguments = ();
        async fn pre_start(&self, _: ActorRef<T>, _: ()) -> Result<(), ractor::ActorProcessingErr> {
            Ok(())
        }
        async fn handle(
            &self,
            _: ActorRef<T>,
            message: T,
            _: &mut (),
        ) -> Result<(), ractor::ActorProcessingErr> {
            self.0.lock().unwrap().push(message);
            Ok(())
        }
    }

    struct Rig {
        service: String,
        handle: AuthorityHandle,
        changes: Arc<Mutex<Vec<AuthorityChanged>>>,
        asks: Arc<Mutex<Vec<AuthorityAsk>>>,
    }

    async fn rig() -> Rig {
        struct Parent;
        #[ractor::async_trait]
        impl Actor for Parent {
            type Msg = ();
            type State = ();
            type Arguments = ();
            async fn pre_start(
                &self,
                _: ActorRef<()>,
                _: (),
            ) -> Result<(), ractor::ActorProcessingErr> {
                Ok(())
            }
        }
        let (parent, _) = Actor::spawn(None, Parent, ()).await.unwrap();
        let service = unique_service();
        let handle = AuthorityHandle::spawn(&service, CREDENTIALS, trust(), parent.get_cell())
            .await
            .unwrap();
        let changes = Arc::new(Mutex::new(Vec::new()));
        let asks = Arc::new(Mutex::new(Vec::new()));
        let (c, _) = Actor::spawn(None, Sink(changes.clone()), ()).await.unwrap();
        let (a, _) = Actor::spawn(None, Sink(asks.clone()), ()).await.unwrap();
        handle.changes().subscribe(c, Some);
        handle.asks().subscribe(a, Some);
        Rig {
            service,
            handle,
            changes,
            asks,
        }
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(80)).await;
    }

    /// Actor names are process-wide, and the tests run side by side.
    fn unique_service() -> String {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        format!(
            "authority-test-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        )
    }

    #[tokio::test]
    async fn the_actors_the_handle_makes_are_found_by_the_name_of_the_service() {
        struct Parent;
        #[ractor::async_trait]
        impl Actor for Parent {
            type Msg = ();
            type State = ();
            type Arguments = ();
            async fn pre_start(
                &self,
                _: ActorRef<()>,
                _: (),
            ) -> Result<(), ractor::ActorProcessingErr> {
                Ok(())
            }
        }
        let (parent, _) = Actor::spawn(None, Parent, ()).await.unwrap();
        let service = unique_service();
        let lookup = PolicyLookup::for_service(&service);
        assert!(lookup.actor().is_none());
        let handle = AuthorityHandle::spawn(&service, CREDENTIALS, trust(), parent.get_cell())
            .await
            .unwrap();
        assert_eq!(lookup.actor().unwrap().get_id(), handle.policy().get_id());
        assert_eq!(
            ractor::registry::where_is(permissions_actor_name(&service))
                .unwrap()
                .get_id(),
            handle.permissions().get_id()
        );
        // Another agent's name finds nothing.
        assert!(
            PolicyLookup::for_service("some-other-agent")
                .actor()
                .is_none()
        );
    }

    #[tokio::test]
    async fn asking_before_the_policy_exists_is_an_error_and_never_a_decision() {
        let lookup = PolicyLookup::for_service("nobody-has-started-this");
        let answer = lookup
            .check(
                "alice",
                None,
                r#"Action::"run-command""#,
                r#"Command::"a/b""#,
                Default::default(),
            )
            .await;
        assert!(answer.unwrap_err().contains("not running yet"));
    }

    #[tokio::test]
    async fn a_policy_that_is_running_with_nothing_loaded_is_told_apart_from_one_that_is_not_running()
     {
        // Nothing is running under this name: the lookup cannot reach a policy at all.
        let absent = PolicyLookup::for_service("nobody-has-started-this-either")
            .decide(
                "alice",
                None,
                r#"Action::"run-command""#,
                r#"Command::"a/b""#,
                Default::default(),
            )
            .await;
        assert!(
            matches!(&absent, Err(LookupError::Unavailable(why)) if why.contains("not running yet")),
            "{absent:?}"
        );

        // The policy actor is running and has been given no rules: that is a state a caller can act on.
        let rig = rig().await;
        let lookup = PolicyLookup::named(rig.service.clone() + ".policy");
        let empty = lookup
            .decide(
                "alice",
                None,
                r#"Action::"run-command""#,
                r#"Command::"a/b""#,
                Default::default(),
            )
            .await;
        assert_eq!(empty, Err(LookupError::NoPolicyLoaded));
    }

    #[tokio::test]
    async fn the_string_form_of_the_lookup_says_what_it_always_did() {
        let rig = rig().await;
        let lookup = PolicyLookup::named(rig.service.clone() + ".policy");
        let answer = lookup
            .check(
                "alice",
                None,
                r#"Action::"run-command""#,
                r#"Command::"a/b""#,
                Default::default(),
            )
            .await;
        assert_eq!(answer.unwrap_err(), "no policy set has been loaded yet");
    }

    #[tokio::test]
    async fn a_request_policy_cannot_build_is_unavailable_and_not_mistaken_for_no_policy() {
        let rig = rig().await;
        let lookup = PolicyLookup::named(rig.service.clone() + ".policy");
        rig.handle.snapshot(
            snapshot(&[policy(
                1,
                1,
                r#"permit(principal == Identity::"alice", action == Action::"run-command", resource);"#,
            )]),
            Some(CREDENTIALS.into()),
        );
        settle().await;
        // Rules are loaded, but this resource is not an entity: policy fails, and that is no answer.
        let answer = lookup
            .decide(
                "alice",
                None,
                r#"Action::"run-command""#,
                "this is not an entity",
                Default::default(),
            )
            .await;
        assert!(
            matches!(answer, Err(LookupError::Unavailable(_))),
            "{answer:?}"
        );
    }

    #[tokio::test]
    async fn once_rules_are_loaded_the_typed_lookup_gives_the_decision() {
        let rig = rig().await;
        let lookup = PolicyLookup::named(rig.service.clone() + ".policy");
        rig.handle.snapshot(
            snapshot(&[policy(
                1,
                1,
                r#"permit(principal == Identity::"alice", action == Action::"run-command", resource);"#,
            )]),
            Some(CREDENTIALS.into()),
        );
        settle().await;
        let decision = lookup
            .decide(
                "alice",
                None,
                r#"Action::"run-command""#,
                r#"Command::"a/b""#,
                Default::default(),
            )
            .await;
        assert_eq!(decision, Ok(polar_policy::Decision::Allow));
    }

    #[tokio::test]
    async fn asking_a_running_policy_gives_its_decision_through_the_lookup() {
        let rig = rig().await;
        let lookup = PolicyLookup::named(rig.service.clone() + ".policy");
        rig.handle.snapshot(
            snapshot(&[policy(
                1,
                1,
                r#"permit(principal == Identity::"alice", action == Action::"run-command", resource);"#,
            )]),
            Some(CREDENTIALS.into()),
        );
        settle().await;
        let ask = |who: &'static str| {
            let lookup = lookup.clone();
            async move {
                lookup
                    .check(
                        who,
                        None,
                        r#"Action::"run-command""#,
                        r#"Command::"a/b""#,
                        Default::default(),
                    )
                    .await
                    .unwrap()
            }
        };
        assert_eq!(ask("alice").await, polar_policy::Decision::Allow);
        assert_ne!(ask("mallory").await, polar_policy::Decision::Allow);
    }

    async fn decision(
        rig: &Rig,
        who: &str,
        action: &str,
        resource: &str,
    ) -> polar_policy::Decision {
        ractor::call!(rig.handle.policy(), |reply| {
            polar_policy::PolicyMessage::CheckAuthorization {
                facts: Default::default(),
                requester_identity: who.to_string(),
                requester_certificate_pem: None,
                action: action.to_string(),
                resource: resource.to_string(),
                reply,
            }
        })
        .unwrap()
        .unwrap()
    }

    #[tokio::test]
    async fn a_policy_that_arrives_is_what_the_agents_policy_actor_then_decides_by() {
        let rig = rig().await;
        rig.handle.snapshot(
            snapshot(&[policy(
                1,
                1,
                r#"permit(principal == Identity::"alice", action == Action::"update-runtime", resource);"#,
            )]),
            Some(CREDENTIALS.into()),
        );
        settle().await;
        let allowed = decision(
            &rig,
            "alice",
            r#"Action::"update-runtime""#,
            r#"Section::"a/b""#,
        )
        .await;
        let silent = decision(
            &rig,
            "bob",
            r#"Action::"update-runtime""#,
            r#"Section::"a/b""#,
        )
        .await;
        assert_eq!(allowed, polar_policy::Decision::Allow);
        assert_eq!(silent, polar_policy::Decision::NotDetermined);
    }

    #[tokio::test]
    async fn groups_that_arrive_are_what_the_permissions_actor_then_resolves() {
        let rig = rig().await;
        rig.handle.snapshot(
            snapshot(&[payload(
                1,
                1,
                AuthorityBody::Groups {
                    memberships: [("alice".to_string(), vec!["operators".to_string()])].into(),
                    hierarchy: Default::default(),
                },
            )]),
            Some(CREDENTIALS.into()),
        );
        settle().await;
        let principal = ractor::call!(rig.handle.permissions(), |reply| {
            polar_permissions::PermissionsMessage::ResolvePrincipal {
                identity: "alice".to_string(),
                certificate_pem: None,
                reply,
            }
        })
        .unwrap()
        .unwrap();
        assert_eq!(principal.groups, vec!["operators".to_string()]);
    }

    #[tokio::test]
    async fn revoked_serials_that_arrive_are_what_signer_verification_then_refuses() {
        use crate::signed::Revocation;
        let rig = rig().await;
        let signer = |serial: &str| polar_signing::SignerInfo {
            username: Some("alice".into()),
            email: None,
            identity: None,
            serial: serial.to_string(),
            capabilities: vec![],
        };
        assert!(!rig.handle.revoked().is_revoked(&signer("01ab")));
        rig.handle.snapshot(
            snapshot(&[payload(
                1,
                1,
                AuthorityBody::Revoked {
                    serials: vec!["01ab".into()],
                    compromised: Vec::new(),
                },
            )]),
            Some(CREDENTIALS.into()),
        );
        settle().await;
        assert!(rig.handle.revoked().is_revoked(&signer("01ab")));
        assert!(!rig.handle.revoked().is_revoked(&signer("02cd")));
    }

    /// The revoked set an agent holds (the broker's view among them) knows why each serial was revoked, as the signed
    /// set says: a compromise, or an ordinary end where it says nothing (docs/architecture/authority-model.md,
    /// "Multi-instance workloads").
    #[tokio::test]
    async fn the_revoked_set_held_says_which_were_revoked_for_a_compromise() {
        let rig = rig().await;
        rig.handle.snapshot(
            snapshot(&[payload(
                1,
                1,
                AuthorityBody::Revoked {
                    serials: vec!["01ab".into(), "02cd".into()],
                    compromised: vec!["02cd".into()],
                },
            )]),
            Some(CREDENTIALS.into()),
        );
        settle().await;
        let revoked = rig.handle.revoked();
        assert_eq!(revoked.revoked_for("01ab"), Some(RevokedFor::Ordinary));
        assert_eq!(revoked.revoked_for("02cd"), Some(RevokedFor::Compromise));
        assert_eq!(revoked.revoked_for("03ef"), None);
    }

    #[tokio::test]
    async fn a_change_is_announced_once_per_update_applied_and_not_for_what_is_already_known() {
        let rig = rig().await;
        let snap = snapshot(&[policy(1, 1, "// v1")]);
        rig.handle.snapshot(snap.clone(), Some(CREDENTIALS.into()));
        rig.handle.snapshot(snap, Some(CREDENTIALS.into())); // republished
        settle().await;
        assert_eq!(
            *rig.changes.lock().unwrap(),
            [AuthorityChanged {
                kind: AuthorityKind::Policy,
                version: 1
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_agent_with_no_key_asks_for_the_authority_when_it_registers_and_keeps_asking() {
        let rig = rig().await;
        rig.handle.registered();
        settle().await;
        assert_eq!(rig.asks.lock().unwrap().len(), 1);
        // Still no key, and a tick a while later it asks again; one too soon does not.
        rig.handle.tick();
        settle().await;
        assert_eq!(
            rig.asks.lock().unwrap().len(),
            1,
            "too soon after the last question"
        );
        tokio::time::sleep(Duration::from_secs(6)).await;
        rig.handle.tick();
        settle().await;
        assert_eq!(rig.asks.lock().unwrap().len(), 2);
        // With a key it does not.
        rig.handle.snapshot(snapshot(&[]), Some(CREDENTIALS.into()));
        settle().await;
        tokio::time::sleep(Duration::from_secs(6)).await;
        rig.handle.tick();
        settle().await;
        assert_eq!(rig.asks.lock().unwrap().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn an_update_before_there_is_a_key_asks_and_asks_no_more_than_once_in_the_interval() {
        let rig = rig().await;
        let update = sign(&policy(1, 1, "// v1"), &key()).unwrap();
        rig.handle.update(update.clone(), Some(CREDENTIALS.into()));
        rig.handle.update(update.clone(), Some(CREDENTIALS.into()));
        rig.handle.update(update, Some(CREDENTIALS.into()));
        settle().await;
        assert_eq!(
            rig.asks.lock().unwrap().len(),
            1,
            "three in a burst are one question, not three"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_its_gate_did_not_predict_asks_for_the_authority_even_with_a_key_once_in_the_interval()
     {
        let rig = rig().await;
        rig.handle.snapshot(snapshot(&[]), Some(CREDENTIALS.into()));
        settle().await;
        let before = rig.asks.lock().unwrap().len();
        // A burst of refusals is one question.
        rig.handle.behind();
        rig.handle.behind();
        rig.handle.behind();
        settle().await;
        assert_eq!(rig.asks.lock().unwrap().len(), before + 1);
        tokio::time::sleep(Duration::from_secs(6)).await;
        rig.handle.behind();
        settle().await;
        assert_eq!(rig.asks.lock().unwrap().len(), before + 2);
    }

    #[tokio::test]
    async fn anything_from_anyone_else_changes_nothing() {
        let rig = rig().await;
        rig.handle.snapshot(
            snapshot(&[policy(1, 1, "// v1")]),
            Some("someone-else".into()),
        );
        settle().await;
        assert!(rig.changes.lock().unwrap().is_empty());
        assert!(rig.handle.versions().await.unwrap().is_empty());
    }
}
