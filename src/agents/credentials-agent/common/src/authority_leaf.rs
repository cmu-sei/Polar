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

//! The leaf's side of authority distribution (docs/architecture/authority-distribution.md): what an agent
//! does with a signed update or a snapshot from the credentials agent. Here, with no actors, so the custody daemon
//! keeps the credentials agent's authority by the same rules as every agent (docs/architecture/key-custody.md, step
//! 5); `polar_agent_builder::authority` holds the actor around it.
//!
//! [`AuthorityCore`] is the decision, with no actors in it: given what arrived and who the broker says sent it,
//! it says what to do. In order, for anything that arrives: it must be from the credentials agent's own
//! identity (cheap, and it keeps anyone else from making a leaf do signature work); the key must be one this
//! leaf has pinned, or be pinned now from a snapshot; then each update is verified and applied.
//!
//! The key is pinned from the first snapshot that comes from the credentials agent and carries a certificate for it,
//! believed under the CA the leaf pins (docs/architecture/authority-distribution.md, "Pinning the key to the CA"), and
//! never replaced: a snapshot that names another key is refused and reported, certified or not, because a key that
//! changes under a running system is an incident and not an update.

use crate::authority_state::{Applied, AuthorityState};
use crate::authority_update::{
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
    SetGroups(
        BTreeMap<String, Vec<String>>,
        cert_issuer_common::hierarchy::Hierarchy,
    ),
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
    /// The snapshot's key is not certified as the credentials agent's: nothing in it is believed, nothing is pinned.
    Uncertified(String),
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
    /// The CA the leaf pins, under which the credentials agent's key is certified.
    trust: polar_signing::Trust,
    key: Option<RsaPublicKey>,
    state: AuthorityState,
}

impl AuthorityCore {
    /// `publisher` is the identity the broker gives the credentials agent; `trust`, the CA the leaf pins.
    pub fn new(publisher: impl Into<String>, trust: polar_signing::Trust) -> Self {
        Self {
            publisher: publisher.into(),
            trust,
            key: None,
            state: AuthorityState::default(),
        }
    }

    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    /// The credentials agent's key, as pinned from its first snapshot under the CA: what the custody daemon seals a
    /// new escrow key to, and nothing a caller names (docs/architecture/key-custody.md, "Part A's bootstrap: the
    /// custodian").
    pub fn pinned_key(&self) -> Option<&RsaPublicKey> {
        self.key.as_ref()
    }

    pub fn state(&self) -> &AuthorityState {
        &self.state
    }

    // Whether a message came from the credentials agent: a check, not a constructor.
    #[allow(clippy::wrong_self_convention)]
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

    /// The current authority, whether asked for or republished, at `now` (Unix seconds).
    pub fn on_snapshot(
        &mut self,
        snapshot: &AuthoritySnapshot,
        published_by: Option<&str>,
        now: i64,
    ) -> Result<Handled, Refusal> {
        self.from_the_credentials_agent(published_by)?;
        snapshot
            .public_key()
            .map_err(|e| Refusal::BadKey(e.to_string()))?;
        let carried = snapshot
            .certified_key(&self.trust, &self.publisher, now)
            .map_err(|e| Refusal::Uncertified(e.to_string()))?;
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
            AuthorityBody::Groups {
                memberships,
                hierarchy,
            } => Effect::SetGroups(memberships.clone(), hierarchy.clone()),
            AuthorityBody::Revoked { serials } => Effect::SetRevoked(serials.clone()),
        });
        effects.push(Effect::Changed {
            kind: body.kind(),
            version,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority_update::{AuthorityPayload, sign};
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
                hierarchy: Default::default(),
            },
            authored_by: None,
        }
    }

    fn ca() -> &'static polar_signing::testing::Authority {
        static CA: std::sync::OnceLock<polar_signing::testing::Authority> =
            std::sync::OnceLock::new();
        CA.get_or_init(|| polar_signing::testing::root("Polar Internal CA"))
    }

    fn now() -> i64 {
        polar_signing::testing::now()
    }

    /// A leaf of the cluster: it believes the credentials agent, under the CA it pins.
    fn core() -> AuthorityCore {
        AuthorityCore::new(
            CREDENTIALS,
            polar_signing::Trust::from_pem(&[ca().pem.clone()]).unwrap(),
        )
    }

    /// A signing certificate for `key`, naming `identity`, as cert-issuer issues the credentials agent's.
    fn certificate_for(key: &RsaPrivateKey, identity: &'static str) -> Vec<String> {
        vec![polar_signing::testing::escrow_certificate_for(
            ca(),
            polar_signing::testing::authority_spec(identity),
            &pem(key),
        )]
    }

    /// A snapshot signed by `signer`, under its certificate as the credentials agent's.
    fn snapshot(signer: &RsaPrivateKey, payloads: &[AuthorityPayload]) -> AuthoritySnapshot {
        AuthoritySnapshot {
            public_key_pem: pem(signer),
            certificate_chain_pem: certificate_for(signer, CREDENTIALS),
            updates: payloads.iter().map(|p| sign(p, signer).unwrap()).collect(),
        }
    }

    fn pinned(core: &mut AuthorityCore) {
        core.on_snapshot(&snapshot(&key(0), &[]), Some(CREDENTIALS), now())
            .unwrap();
    }

    #[test]
    fn only_the_credentials_agent_is_listened_to() {
        let mut core = core();
        let snap = snapshot(&key(0), &[policy(1, 1)]);
        for who in [
            None,
            Some("some-other-agent"),
            Some("credentials-agent-consumer"),
        ] {
            assert!(
                matches!(
                    core.on_snapshot(&snap, who, now()),
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
        let mut core = core();
        let update = sign(&policy(1, 1), &key(0)).unwrap();
        assert_eq!(
            core.on_update(&update, Some(CREDENTIALS)),
            Err(Refusal::NoKeyYet)
        );
    }

    #[test]
    fn a_snapshot_pins_the_key_and_applies_what_it_carries() {
        let mut core = core();
        let handled = core
            .on_snapshot(
                &snapshot(&key(0), &[policy(1, 2), groups(3, 1)]),
                Some(CREDENTIALS),
                now(),
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
                Effect::SetGroups(
                    [("alice".to_string(), vec!["operators".to_string()])].into(),
                    Default::default()
                ),
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
        let mut core = core();
        let snap = snapshot(&key(0), &[policy(1, 2)]);
        core.on_snapshot(&snap, Some(CREDENTIALS), now()).unwrap();
        let again = core.on_snapshot(&snap, Some(CREDENTIALS), now()).unwrap();
        assert!(
            again.effects.is_empty() && again.refused.is_empty(),
            "{again:?}"
        );
    }

    #[test]
    fn a_live_update_is_applied_once_and_an_old_one_not_at_all() {
        let mut core = core();
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
        let mut core = core();
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
                .any(|e| matches!(e, Effect::SetGroups(..))),
            "the update that arrived is applied all the same"
        );
    }

    #[test]
    fn revoked_serials_are_a_kind_of_their_own() {
        let mut core = core();
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
        let mut core = core();
        pinned(&mut core);
        let forged = sign(&policy(1, 1), &key(1)).unwrap();
        assert!(matches!(
            core.on_update(&forged, Some(CREDENTIALS)),
            Err(Refusal::Update(AuthorityUpdateError::UnknownKey { .. }))
        ));
    }

    #[test]
    fn a_snapshot_that_names_a_different_key_is_refused_and_nothing_in_it_is_applied() {
        let mut core = core();
        pinned(&mut core);
        let other = snapshot(&key(1), &[policy(1, 9)]);
        match core.on_snapshot(&other, Some(CREDENTIALS), now()) {
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
        let mut core = core();
        let mut snap = snapshot(&key(0), &[policy(1, 1), groups(2, 1)]);
        snap.updates[0].signature[0] ^= 1;
        let handled = core.on_snapshot(&snap, Some(CREDENTIALS), now()).unwrap();
        assert_eq!(handled.refused, vec![AuthorityUpdateError::BadSignature]);
        assert!(
            handled
                .effects
                .iter()
                .any(|e| matches!(e, Effect::SetGroups(..)))
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
        let mut core = core();
        let snap = AuthoritySnapshot {
            public_key_pem: "nope".into(),
            certificate_chain_pem: certificate_for(&key(0), CREDENTIALS),
            updates: vec![],
        };
        assert!(matches!(
            core.on_snapshot(&snap, Some(CREDENTIALS), now()),
            Err(Refusal::BadKey(_))
        ));
        assert!(!core.has_key());
    }

    #[test]
    fn a_snapshot_whose_key_is_not_certified_as_the_credentials_agent_s_pins_nothing() {
        let other_ca = polar_signing::testing::root("Polar Internal CA");
        let mut cases = vec![
            ("no certificate", {
                let mut s = snapshot(&key(0), &[policy(1, 1)]);
                s.certificate_chain_pem = vec![];
                s
            }),
            ("another identity's", {
                let mut s = snapshot(&key(0), &[policy(1, 1)]);
                s.certificate_chain_pem = certificate_for(
                    &key(0),
                    "git-repo-observer.polar.serviceaccount.cluster.local",
                );
                s
            }),
            ("another key's", {
                let mut s = snapshot(&key(0), &[policy(1, 1)]);
                s.certificate_chain_pem = certificate_for(&key(1), CREDENTIALS);
                s
            }),
        ];
        cases.push(("under a CA the leaf does not pin", {
            let mut s = snapshot(&key(0), &[policy(1, 1)]);
            s.certificate_chain_pem = vec![polar_signing::testing::escrow_certificate_for(
                &other_ca,
                polar_signing::testing::authority_spec(CREDENTIALS),
                &pem(&key(0)),
            )];
            s
        }));
        for (what, snap) in cases {
            let mut core = core();
            assert!(
                matches!(
                    core.on_snapshot(&snap, Some(CREDENTIALS), now()),
                    Err(Refusal::Uncertified(_))
                ),
                "{what}"
            );
            assert!(!core.has_key(), "{what}: nothing pinned");
            assert!(core.state().held(AuthorityKind::Policy).is_none());
        }
        // Out of date: the same certificate, at a time it does not cover.
        let mut core = core();
        let in_2101 = 4_134_000_000;
        assert!(matches!(
            core.on_snapshot(&snapshot(&key(0), &[]), Some(CREDENTIALS), in_2101),
            Err(Refusal::Uncertified(_))
        ));
        assert!(!core.has_key());
    }
}
