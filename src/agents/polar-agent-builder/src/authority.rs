//! The leaf's side of authority distribution (docs/architecture/authority-distribution.md): what an agent
//! does with a signed update or a snapshot from the credentials agent.
//!
//! [`AuthorityCore`] is the decision, with no actors in it: given what arrived and who the broker says sent it,
//! it says what to do. In order, for anything that arrives: it must be from the credentials agent's own
//! identity (cheap, and it keeps anyone else from making a leaf do signature work); the key must be one this
//! leaf has pinned, or be pinned now from a snapshot; then each update is verified and applied.
//!
//! The key is pinned from the first snapshot that comes from the credentials agent, and never replaced: a
//! snapshot that names another key is refused and reported, because a key that changes under a running system is
//! an incident and not an update.

use credentials_agent_common::authority_state::{Applied, AuthorityState};
use credentials_agent_common::authority_update::{
    AuthorityBody, AuthorityKind, AuthoritySnapshot, AuthorityUpdate, AuthorityUpdateError,
    fingerprint, verify,
};
use rsa::RsaPublicKey;
use std::collections::BTreeMap;

/// What to do about an update that was believed. The actor carries these out, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Replace the policy rules.
    SetPolicy(String),
    /// Replace group membership.
    SetGroups(BTreeMap<String, Vec<String>>),
    /// Replace the set of revoked certificate serials.
    SetRevoked(Vec<String>),
    /// Tell the agent's own actors that authority of this kind changed.
    Changed { kind: AuthorityKind, version: u64 },
    /// An update was missed: ask the credentials agent for the current authority.
    AskForSnapshot,
}

/// Why a whole message was not acted on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Not from the credentials agent. `got` is who the broker says sent it.
    WrongPublisher { got: Option<String> },
    /// No key is pinned yet, so an update cannot be checked. A snapshot will bring one.
    NoKeyYet,
    /// The snapshot's key is not the one pinned.
    KeyChanged { pinned: String, got: String },
    /// The snapshot's key is not a key.
    BadKey(String),
    /// An update did not verify.
    Update(AuthorityUpdateError),
}

/// What came of a message that was acted on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Handled {
    pub effects: Vec<Effect>,
    /// Updates inside a snapshot that did not verify; the others were applied.
    pub refused: Vec<AuthorityUpdateError>,
}

pub struct AuthorityCore {
    publisher: String,
    key: Option<RsaPublicKey>,
    state: AuthorityState,
}

impl AuthorityCore {
    /// `publisher` is the identity the broker gives the credentials agent.
    pub fn new(publisher: impl Into<String>) -> Self {
        Self {
            publisher: publisher.into(),
            key: None,
            state: AuthorityState::default(),
        }
    }

    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    pub fn state(&self) -> &AuthorityState {
        &self.state
    }

    fn from_the_credentials_agent(&self, published_by: Option<&str>) -> Result<(), Refusal> {
        match published_by {
            Some(who) if who == self.publisher => Ok(()),
            other => Err(Refusal::WrongPublisher {
                got: other.map(str::to_string),
            }),
        }
    }

    /// A live update.
    pub fn on_update(
        &mut self,
        update: &AuthorityUpdate,
        published_by: Option<&str>,
    ) -> Result<Handled, Refusal> {
        self.from_the_credentials_agent(published_by)?;
        let key = self.key.as_ref().ok_or(Refusal::NoKeyYet)?;
        let payload = verify(update, key).map_err(Refusal::Update)?;
        let outcome = self.state.apply(payload.clone(), false);
        let mut handled = Handled::default();
        if outcome.applied == Applied::Replaced {
            Self::effects_of(&payload.body, payload.version, &mut handled.effects);
        }
        if outcome.missed_an_update {
            handled.effects.push(Effect::AskForSnapshot);
        }
        Ok(handled)
    }

    /// The current authority, whether asked for or republished.
    pub fn on_snapshot(
        &mut self,
        snapshot: &AuthoritySnapshot,
        published_by: Option<&str>,
    ) -> Result<Handled, Refusal> {
        self.from_the_credentials_agent(published_by)?;
        let carried = snapshot
            .public_key()
            .map_err(|e| Refusal::BadKey(e.to_string()))?;
        match &self.key {
            Some(pinned) if pinned != &carried => {
                return Err(Refusal::KeyChanged {
                    pinned: fingerprint(pinned).map_err(Refusal::Update)?,
                    got: fingerprint(&carried).map_err(Refusal::Update)?,
                });
            }
            Some(_) => {}
            // The first snapshot from the credentials agent's own identity sets the key for good.
            None => self.key = Some(carried),
        }
        let key = self.key.as_ref().expect("pinned just above");

        let mut handled = Handled::default();
        for update in &snapshot.updates {
            match verify(update, key) {
                Err(e) => handled.refused.push(e),
                Ok(payload) => {
                    // What a snapshot carries jumps in sequence by nature: not a miss.
                    let outcome = self.state.apply(payload.clone(), true);
                    if outcome.applied == Applied::Replaced {
                        Self::effects_of(&payload.body, payload.version, &mut handled.effects);
                    }
                }
            }
        }
        Ok(handled)
    }

    fn effects_of(body: &AuthorityBody, version: u64, effects: &mut Vec<Effect>) {
        effects.push(match body {
            AuthorityBody::Policy { source } => Effect::SetPolicy(source.clone()),
            AuthorityBody::Groups { memberships } => Effect::SetGroups(memberships.clone()),
            AuthorityBody::Revoked { serials } => Effect::SetRevoked(serials.clone()),
        });
        effects.push(Effect::Changed {
            kind: body.kind(),
            version,
        });
    }
}

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
pub struct RevokedSerials(Arc<RwLock<HashSet<String>>>);

impl RevokedSerials {
    fn replace(&self, serials: Vec<String>) {
        *self
            .0
            .write()
            .expect("the revoked set is never held across a panic") = serials.into_iter().collect();
    }
}

impl Revocation for RevokedSerials {
    fn is_revoked(&self, signer: &polar_signing::SignerInfo) -> bool {
        self.0
            .read()
            .expect("the revoked set is never held across a panic")
            .contains(&signer.serial)
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
                Effect::SetGroups(memberships) => {
                    let memberships: HashMap<String, Vec<String>> =
                        memberships.into_iter().collect();
                    if let Err(e) = ractor::call!(self.args.permissions, |reply| {
                        PermissionsMessage::UpdateGroups { memberships, reply }
                    }) {
                        error!("the permissions actor did not take the groups: {e}");
                    }
                }
                Effect::SetRevoked(serials) => self.args.revoked.replace(serials),
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
    type Arguments = AuthorityArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: AuthorityArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        // While there is no key it asks again from time to time; with one the tick does nothing.
        drop(myself.send_interval(TICK, || AuthorityMessage::Tick));
        Ok(AuthorityActorState {
            core: AuthorityCore::new(args.publisher.clone()),
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
                let result = state.core.on_snapshot(&snapshot, published_by.as_deref());
                state.handled(result).await;
            }
            AuthorityMessage::Registered => state.ask(),
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
    /// children of `supervisor`. `publisher` is the identity the broker gives the credentials agent.
    pub async fn spawn(
        publisher: impl Into<String>,
        supervisor: ActorCell,
    ) -> Result<Self, ActorProcessingErr> {
        let (permissions, _) =
            Actor::spawn_linked(None, PermissionsActor, (), supervisor.clone()).await?;
        let (policy, _) = Actor::spawn_linked(
            None,
            PolicyActor,
            PolicyActorArgs {
                permissions_actor: permissions.clone(),
            },
            supervisor.clone(),
        )
        .await?;
        Self::spawn_over(publisher, permissions, policy, supervisor).await
    }

    /// The same, over a permissions actor and a policy actor the agent already made: for an agent that needs
    /// its policy before the builder exists (the credentials agent builds the handlers that enforce policy
    /// first). The actors are kept current exactly as ones the handle made would be. They are not stopped
    /// by this handle, nor linked to `supervisor`: they are the agent's.
    pub async fn spawn_over(
        publisher: impl Into<String>,
        permissions: ActorRef<PermissionsMessage>,
        policy: ActorRef<PolicyMessage>,
        supervisor: ActorCell,
    ) -> Result<Self, ActorProcessingErr> {
        let revoked = RevokedSerials::default();
        let changes = Arc::new(OutputPort::default());
        let asks = Arc::new(OutputPort::default());
        let (actor, _) = Actor::spawn_linked(
            None,
            AuthorityActor,
            AuthorityArgs {
                publisher: publisher.into(),
                permissions: permissions.clone(),
                policy: policy.clone(),
                revoked: revoked.clone(),
                changes: changes.clone(),
                asks: asks.clone(),
            },
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
mod tests {
    use super::*;
    use credentials_agent_common::authority_update::{AuthorityPayload, sign};
    use rsa::RsaPrivateKey;
    use rsa::pkcs8::EncodePublicKey;

    const CREDENTIALS: &str = "credentials-agent.polar.serviceaccount.cluster.local";

    fn key(n: usize) -> RsaPrivateKey {
        use std::sync::OnceLock;
        static KEYS: OnceLock<[RsaPrivateKey; 2]> = OnceLock::new();
        KEYS.get_or_init(|| {
            [
                RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap(),
                RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap(),
            ]
        })[n]
            .clone()
    }

    fn pem(key: &RsaPrivateKey) -> String {
        key.to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap()
    }

    fn policy(sequence: u64, version: u64) -> AuthorityPayload {
        AuthorityPayload {
            sequence,
            version,
            body: AuthorityBody::Policy {
                source: format!("// v{version}"),
            },
            authored_by: None,
        }
    }

    fn groups(sequence: u64, version: u64) -> AuthorityPayload {
        AuthorityPayload {
            sequence,
            version,
            body: AuthorityBody::Groups {
                memberships: [("alice".to_string(), vec!["operators".to_string()])].into(),
            },
            authored_by: None,
        }
    }

    fn snapshot(signer: &RsaPrivateKey, payloads: &[AuthorityPayload]) -> AuthoritySnapshot {
        AuthoritySnapshot {
            public_key_pem: pem(signer),
            updates: payloads.iter().map(|p| sign(p, signer).unwrap()).collect(),
        }
    }

    fn pinned(core: &mut AuthorityCore) {
        core.on_snapshot(&snapshot(&key(0), &[]), Some(CREDENTIALS))
            .unwrap();
    }

    #[test]
    fn only_the_credentials_agent_is_listened_to() {
        let mut core = AuthorityCore::new(CREDENTIALS);
        let snap = snapshot(&key(0), &[policy(1, 1)]);
        for who in [
            None,
            Some("some-other-agent"),
            Some("credentials-agent-consumer"),
        ] {
            assert!(
                matches!(
                    core.on_snapshot(&snap, who),
                    Err(Refusal::WrongPublisher { .. })
                ),
                "{who:?}"
            );
        }
        assert!(!core.has_key(), "nothing was pinned from a stranger");

        pinned(&mut core);
        let update = sign(&policy(1, 1), &key(0)).unwrap();
        assert!(matches!(
            core.on_update(&update, Some("some-other-agent")),
            Err(Refusal::WrongPublisher { got: Some(g) }) if g == "some-other-agent"
        ));
    }

    #[test]
    fn an_update_cannot_be_checked_before_a_key_is_pinned() {
        let mut core = AuthorityCore::new(CREDENTIALS);
        let update = sign(&policy(1, 1), &key(0)).unwrap();
        assert_eq!(
            core.on_update(&update, Some(CREDENTIALS)),
            Err(Refusal::NoKeyYet)
        );
    }

    #[test]
    fn a_snapshot_pins_the_key_and_applies_what_it_carries() {
        let mut core = AuthorityCore::new(CREDENTIALS);
        let handled = core
            .on_snapshot(
                &snapshot(&key(0), &[policy(1, 2), groups(3, 1)]),
                Some(CREDENTIALS),
            )
            .unwrap();
        assert!(core.has_key());
        assert!(handled.refused.is_empty());
        assert_eq!(
            handled.effects,
            vec![
                Effect::SetPolicy("// v2".into()),
                Effect::Changed {
                    kind: AuthorityKind::Policy,
                    version: 2
                },
                Effect::SetGroups([("alice".to_string(), vec!["operators".to_string()])].into()),
                Effect::Changed {
                    kind: AuthorityKind::Groups,
                    version: 1
                },
            ]
        );
        // What the snapshot jumped over (sequence 2) is not a miss.
        assert!(!handled.effects.contains(&Effect::AskForSnapshot));
    }

    #[test]
    fn a_snapshot_republished_changes_nothing() {
        let mut core = AuthorityCore::new(CREDENTIALS);
        let snap = snapshot(&key(0), &[policy(1, 2)]);
        core.on_snapshot(&snap, Some(CREDENTIALS)).unwrap();
        let again = core.on_snapshot(&snap, Some(CREDENTIALS)).unwrap();
        assert!(
            again.effects.is_empty() && again.refused.is_empty(),
            "{again:?}"
        );
    }

    #[test]
    fn a_live_update_is_applied_once_and_an_old_one_not_at_all() {
        let mut core = AuthorityCore::new(CREDENTIALS);
        pinned(&mut core);
        let v1 = sign(&policy(1, 1), &key(0)).unwrap();
        let v2 = sign(&policy(2, 2), &key(0)).unwrap();
        assert!(
            !core
                .on_update(&v1, Some(CREDENTIALS))
                .unwrap()
                .effects
                .is_empty()
        );
        assert!(
            !core
                .on_update(&v2, Some(CREDENTIALS))
                .unwrap()
                .effects
                .is_empty()
        );
        assert!(
            core.on_update(&v1, Some(CREDENTIALS))
                .unwrap()
                .effects
                .is_empty(),
            "version 1 after version 2 changes nothing"
        );
    }

    #[test]
    fn a_jump_in_the_sequence_applies_the_update_and_asks_for_the_rest() {
        let mut core = AuthorityCore::new(CREDENTIALS);
        pinned(&mut core);
        core.on_update(&sign(&policy(1, 1), &key(0)).unwrap(), Some(CREDENTIALS))
            .unwrap();
        let handled = core
            .on_update(&sign(&groups(5, 1), &key(0)).unwrap(), Some(CREDENTIALS))
            .unwrap();
        assert!(
            handled.effects.contains(&Effect::AskForSnapshot),
            "{handled:?}"
        );
        assert!(
            handled
                .effects
                .iter()
                .any(|e| matches!(e, Effect::SetGroups(_))),
            "the update that arrived is applied all the same"
        );
    }

    #[test]
    fn revoked_serials_are_a_kind_of_their_own() {
        let mut core = AuthorityCore::new(CREDENTIALS);
        pinned(&mut core);
        let revoked = AuthorityPayload {
            sequence: 1,
            version: 1,
            body: AuthorityBody::Revoked {
                serials: vec!["01ab".into()],
            },
            authored_by: None,
        };
        let handled = core
            .on_update(&sign(&revoked, &key(0)).unwrap(), Some(CREDENTIALS))
            .unwrap();
        assert_eq!(handled.effects[0], Effect::SetRevoked(vec!["01ab".into()]));
    }

    #[test]
    fn an_update_signed_by_another_key_is_refused() {
        let mut core = AuthorityCore::new(CREDENTIALS);
        pinned(&mut core);
        let forged = sign(&policy(1, 1), &key(1)).unwrap();
        assert!(matches!(
            core.on_update(&forged, Some(CREDENTIALS)),
            Err(Refusal::Update(AuthorityUpdateError::UnknownKey { .. }))
        ));
    }

    #[test]
    fn a_snapshot_that_names_a_different_key_is_refused_and_nothing_in_it_is_applied() {
        let mut core = AuthorityCore::new(CREDENTIALS);
        pinned(&mut core);
        let other = snapshot(&key(1), &[policy(1, 9)]);
        match core.on_snapshot(&other, Some(CREDENTIALS)) {
            Err(Refusal::KeyChanged { pinned, got }) => {
                assert_eq!(pinned, fingerprint(&key(0).to_public_key()).unwrap());
                assert_eq!(got, fingerprint(&key(1).to_public_key()).unwrap());
            }
            other => panic!("{other:?}"),
        }
        assert!(core.state().held(AuthorityKind::Policy).is_none());
    }

    #[test]
    fn one_bad_update_in_a_snapshot_does_not_stop_the_good_ones() {
        let mut core = AuthorityCore::new(CREDENTIALS);
        let mut snap = snapshot(&key(0), &[policy(1, 1), groups(2, 1)]);
        snap.updates[0].signature[0] ^= 1;
        let handled = core.on_snapshot(&snap, Some(CREDENTIALS)).unwrap();
        assert_eq!(handled.refused, vec![AuthorityUpdateError::BadSignature]);
        assert!(
            handled
                .effects
                .iter()
                .any(|e| matches!(e, Effect::SetGroups(_)))
        );
        assert!(
            !handled
                .effects
                .iter()
                .any(|e| matches!(e, Effect::SetPolicy(_)))
        );
    }

    #[test]
    fn a_snapshot_with_a_key_that_is_not_a_key_is_refused() {
        let mut core = AuthorityCore::new(CREDENTIALS);
        let snap = AuthoritySnapshot {
            public_key_pem: "nope".into(),
            updates: vec![],
        };
        assert!(matches!(
            core.on_snapshot(&snap, Some(CREDENTIALS)),
            Err(Refusal::BadKey(_))
        ));
        assert!(!core.has_key());
    }
}

#[cfg(test)]
mod actor_tests {
    use super::*;
    use credentials_agent_common::authority_update::{AuthorityPayload, sign};
    use polar_permissions::PermissionsActor;
    use polar_policy::{PolicyActor, PolicyActorArgs};
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

    fn snapshot(payloads: &[AuthorityPayload]) -> AuthoritySnapshot {
        AuthoritySnapshot {
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

    fn policy_payload(sequence: u64, version: u64, source: &str) -> AuthorityPayload {
        policy(sequence, version, source)
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
        let handle = AuthorityHandle::spawn(CREDENTIALS, parent.get_cell())
            .await
            .unwrap();
        let changes = Arc::new(Mutex::new(Vec::new()));
        let asks = Arc::new(Mutex::new(Vec::new()));
        let (c, _) = Actor::spawn(None, Sink(changes.clone()), ()).await.unwrap();
        let (a, _) = Actor::spawn(None, Sink(asks.clone()), ()).await.unwrap();
        handle.changes().subscribe(c, Some);
        handle.asks().subscribe(a, Some);
        Rig {
            handle,
            changes,
            asks,
        }
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(80)).await;
    }

    async fn decision(
        rig: &Rig,
        who: &str,
        action: &str,
        resource: &str,
    ) -> polar_policy::Decision {
        ractor::call!(rig.handle.policy(), |reply| {
            polar_policy::PolicyMessage::CheckAuthorization {
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
    async fn a_handle_over_actors_an_agent_already_made_keeps_those_same_actors_current() {
        // An agent that needs its policy before the builder exists (the credentials agent, whose handlers
        // are built first) makes the actors itself and hands them over.
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
        let (permissions, _) = Actor::spawn(None, PermissionsActor, ()).await.unwrap();
        let (policy, _) = Actor::spawn(
            None,
            PolicyActor,
            PolicyActorArgs {
                permissions_actor: permissions.clone(),
            },
        )
        .await
        .unwrap();
        let handle = AuthorityHandle::spawn_over(
            CREDENTIALS,
            permissions.clone(),
            policy.clone(),
            parent.get_cell(),
        )
        .await
        .unwrap();
        assert_eq!(handle.policy().get_id(), policy.get_id());
        assert_eq!(handle.permissions().get_id(), permissions.get_id());

        handle.snapshot(
            snapshot(&[policy_payload(
                1,
                1,
                r#"permit(principal == Identity::"alice", action == Action::"x", resource);"#,
            )]),
            Some(CREDENTIALS.into()),
        );
        settle().await;
        // Asked of the actor the agent made, not of one the handle made.
        let decision = ractor::call!(policy, |reply| {
            polar_policy::PolicyMessage::CheckAuthorization {
                requester_identity: "alice".to_string(),
                requester_certificate_pem: None,
                action: r#"Action::"x""#.to_string(),
                resource: r#"Thing::"t""#.to_string(),
                reply,
            }
        })
        .unwrap()
        .unwrap();
        assert_eq!(decision, polar_policy::Decision::Allow);
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
                },
            )]),
            Some(CREDENTIALS.into()),
        );
        settle().await;
        assert!(rig.handle.revoked().is_revoked(&signer("01ab")));
        assert!(!rig.handle.revoked().is_revoked(&signer("02cd")));
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
