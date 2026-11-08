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

//! What the credentials agent has signed and holds, to publish and to answer with
//! (docs/architecture/authority-distribution.md): the latest signed update of each kind, and the number of the
//! last one it signed.
//!
//! It is pure. The caller has already decided that something changed, and has had a sequence number assigned
//! (the ledger does that, durably, so the sequence goes on rising across restarts); the book signs, remembers
//! the latest of each kind, and builds the snapshot an agent is answered with.

use crate::authority_update::{
    AuthorityBody, AuthorityKind, AuthorityPayload, AuthoritySnapshot, AuthorityUpdate,
    AuthorityUpdateError, sign,
};
use rsa::RsaPrivateKey;
use rsa::pkcs8::EncodePublicKey;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BookError {
    /// The version is not higher than the one held of its kind: nothing was signed.
    #[error("version {offered} is not higher than the {held} already held")]
    Stale { offered: u64, held: u64 },
    /// The sequence is not above the last one signed: it would let a holder think an update repeated.
    #[error("sequence {offered} is not above the last signed, {last}")]
    SequenceDidNotRise { offered: u64, last: u64 },
    #[error(transparent)]
    Update(#[from] AuthorityUpdateError),
    /// No certificate for the key is held yet, so no snapshot is made: a leaf would not pin its key without one.
    #[error("the signing key has no certificate yet")]
    NotCertified,
    /// The certificate offered is not for the key the book signs with.
    #[error("the certificate is not for the signing key: {0}")]
    NotForTheKey(String),
}

pub struct AuthorityBook {
    key: RsaPrivateKey,
    /// The key's certificate and what leads to the CA, once cert-issuer has issued one
    /// (docs/architecture/authority-distribution.md, "Pinning the key to the CA").
    certificate_chain_pem: Option<Vec<String>>,
    last_sequence: u64,
    latest: HashMap<AuthorityKind, (u64, AuthorityUpdate)>,
}

impl AuthorityBook {
    /// `last_sequence` is the highest the ledger has assigned, so that the next is above it.
    pub fn new(key: RsaPrivateKey, last_sequence: u64) -> Self {
        Self {
            key,
            certificate_chain_pem: None,
            last_sequence,
            latest: HashMap::new(),
        }
    }

    /// Holds `chain` as the signing key's certificate from now on, in place of any before it: refused, and the one
    /// held kept, if its first certificate is not for the key.
    pub fn certify(&mut self, chain: Vec<String>) -> Result<(), BookError> {
        use rsa::pkcs8::DecodePublicKey;
        let leaf = chain
            .first()
            .ok_or_else(|| BookError::NotForTheKey("no certificate".to_string()))?;
        let certified = cert_issuer_common::public_key::extract_public_key_pem(leaf)
            .map_err(|e| BookError::NotForTheKey(e.to_string()))?;
        let certified = rsa::RsaPublicKey::from_public_key_pem(&certified)
            .map_err(|e| BookError::NotForTheKey(format!("not an RSA key: {e}")))?;
        if certified != self.key.to_public_key() {
            return Err(BookError::NotForTheKey("it is for another key".to_string()));
        }
        self.certificate_chain_pem = Some(chain);
        Ok(())
    }

    /// Whether a certificate for the key is held.
    pub fn is_certified(&self) -> bool {
        self.certificate_chain_pem.is_some()
    }

    /// Whether `version` of `kind` would replace what is held. Asked before a sequence is assigned, so that
    /// nothing is spent on what would be refused.
    pub fn would_replace(&self, kind: AuthorityKind, version: u64) -> bool {
        self.latest
            .get(&kind)
            .is_none_or(|(held, _)| version > *held)
    }

    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    /// Signs `body` as `version` of its kind with the `sequence` the ledger assigned, and holds it as the
    /// latest of its kind.
    pub fn publish(
        &mut self,
        body: AuthorityBody,
        version: u64,
        sequence: u64,
        authored_by: Option<String>,
    ) -> Result<AuthorityUpdate, BookError> {
        let kind = body.kind();
        if let Some((held, _)) = self.latest.get(&kind)
            && version <= *held
        {
            return Err(BookError::Stale {
                offered: version,
                held: *held,
            });
        }
        if sequence <= self.last_sequence {
            return Err(BookError::SequenceDidNotRise {
                offered: sequence,
                last: self.last_sequence,
            });
        }
        let update = sign(
            &AuthorityPayload {
                sequence,
                version,
                body,
                authored_by,
            },
            &self.key,
        )?;
        self.last_sequence = sequence;
        self.latest.insert(kind, (version, update.clone()));
        Ok(update)
    }

    /// The signing key and the latest signed update of each kind, oldest sequence first: what an agent is
    /// answered with, and what is republished.
    pub fn snapshot(&self) -> Result<AuthoritySnapshot, BookError> {
        let certificate_chain_pem = self
            .certificate_chain_pem
            .clone()
            .ok_or(BookError::NotCertified)?;
        let public_key_pem = self
            .key
            .to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .map_err(|e| AuthorityUpdateError::Encoding(e.to_string()))?;
        let mut held: Vec<_> = self.latest.values().collect();
        // Held updates are signed bytes; order by the sequence they were signed with.
        held.sort_by_key(|(_, update)| sequence_of(update));
        Ok(AuthoritySnapshot {
            public_key_pem,
            certificate_chain_pem,
            updates: held.into_iter().map(|(_, u)| u.clone()).collect(),
        })
    }
}

fn sequence_of(update: &AuthorityUpdate) -> u64 {
    serde_json::from_slice::<AuthorityPayload>(&update.payload)
        .map(|p| p.sequence)
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority_state::{Applied, AuthorityState};
    use crate::authority_update::verify;

    fn key() -> RsaPrivateKey {
        use std::sync::OnceLock;
        static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
        KEY.get_or_init(|| RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap())
            .clone()
    }

    const CREDENTIALS: &str = "credentials-agent.polar.serviceaccount.cluster.local";

    fn ca() -> &'static polar_signing::testing::Authority {
        static CA: std::sync::OnceLock<polar_signing::testing::Authority> =
            std::sync::OnceLock::new();
        CA.get_or_init(|| polar_signing::testing::root("Polar Internal CA"))
    }

    /// What cert-issuer issues the credentials agent for `key`: its signing certificate.
    fn certificate_for(key: &RsaPrivateKey) -> Vec<String> {
        let public = key
            .to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();
        vec![polar_signing::testing::escrow_certificate_for(
            ca(),
            polar_signing::testing::authority_spec(CREDENTIALS),
            &public,
        )]
    }

    fn certified_book(last_sequence: u64) -> AuthorityBook {
        let mut book = AuthorityBook::new(key(), last_sequence);
        book.certify(certificate_for(&key())).unwrap();
        book
    }

    fn policy(source: &str) -> AuthorityBody {
        AuthorityBody::Policy {
            source: source.into(),
        }
    }

    fn groups() -> AuthorityBody {
        AuthorityBody::Groups {
            memberships: [("alice".to_string(), vec!["operators".to_string()])].into(),
            hierarchy: Default::default(),
        }
    }

    #[test]
    fn what_is_published_is_signed_by_the_key_and_a_holder_believes_it() {
        let mut book = AuthorityBook::new(key(), 0);
        let update = book
            .publish(policy("// v1"), 1, 1, Some("alice".into()))
            .unwrap();
        let payload = verify(&update, &key().to_public_key()).unwrap();
        assert_eq!(payload.sequence, 1);
        assert_eq!(payload.version, 1);
        assert_eq!(payload.authored_by.as_deref(), Some("alice"));
        // A holder takes it.
        let mut holder = AuthorityState::default();
        assert_eq!(holder.apply(payload, false).applied, Applied::Replaced);
    }

    #[test]
    fn a_version_that_would_not_replace_is_refused_and_signs_nothing() {
        let mut book = AuthorityBook::new(key(), 0);
        book.publish(policy("// v2"), 2, 1, None).unwrap();
        assert!(!book.would_replace(AuthorityKind::Policy, 2));
        assert!(!book.would_replace(AuthorityKind::Policy, 1));
        assert!(book.would_replace(AuthorityKind::Policy, 3));
        // Another kind has a version of its own.
        assert!(book.would_replace(AuthorityKind::Groups, 1));
        assert_eq!(
            book.publish(policy("// again"), 2, 2, None),
            Err(BookError::Stale {
                offered: 2,
                held: 2
            })
        );
        assert_eq!(book.last_sequence(), 1, "the sequence was not spent");
    }

    #[test]
    fn the_sequence_must_rise_and_continues_from_where_the_ledger_left_off() {
        // A restarted agent: the ledger says 41 were assigned before.
        let mut book = AuthorityBook::new(key(), 41);
        assert_eq!(
            book.publish(policy("// v1"), 1, 41, None),
            Err(BookError::SequenceDidNotRise {
                offered: 41,
                last: 41
            })
        );
        assert!(book.publish(policy("// v1"), 1, 42, None).is_ok());
        assert_eq!(book.last_sequence(), 42);
    }

    #[test]
    fn no_snapshot_is_made_before_the_key_is_certified_and_one_a_leaf_believes_after() {
        let mut book = AuthorityBook::new(key(), 0);
        assert!(!book.is_certified());
        assert_eq!(book.snapshot(), Err(BookError::NotCertified));
        book.certify(certificate_for(&key())).unwrap();
        assert!(book.is_certified());
        let snapshot = book.snapshot().unwrap();
        let trust = polar_signing::Trust::from_pem(&[ca().pem.clone()]).unwrap();
        assert_eq!(
            snapshot.certified_key(&trust, CREDENTIALS, polar_signing::testing::now()),
            Ok(key().to_public_key())
        );
    }

    #[test]
    fn a_certificate_for_another_key_is_refused_and_the_one_held_kept() {
        let mut book = certified_book(0);
        let held = book.snapshot().unwrap().certificate_chain_pem;
        let other = RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap();
        assert!(matches!(
            book.certify(certificate_for(&other)),
            Err(BookError::NotForTheKey(_))
        ));
        assert!(matches!(
            book.certify(vec![]),
            Err(BookError::NotForTheKey(_))
        ));
        assert_eq!(book.snapshot().unwrap().certificate_chain_pem, held);
    }

    #[test]
    fn a_snapshot_has_the_key_and_the_latest_of_each_kind_oldest_first() {
        let mut book = certified_book(0);
        let empty = book.snapshot().unwrap();
        assert!(empty.updates.is_empty());
        assert_eq!(empty.public_key().unwrap(), key().to_public_key());

        book.publish(policy("// v1"), 1, 1, None).unwrap();
        book.publish(groups(), 1, 2, None).unwrap();
        book.publish(policy("// v2"), 2, 3, None).unwrap();
        let snapshot = book.snapshot().unwrap();
        let payloads: Vec<AuthorityPayload> = snapshot
            .updates
            .iter()
            .map(|u| verify(u, &key().to_public_key()).unwrap())
            .collect();
        // Policy v1 was replaced by v2, so what remains is groups (2) then policy v2 (3).
        assert_eq!(
            payloads
                .iter()
                .map(|p| (p.sequence, p.version))
                .collect::<Vec<_>>(),
            [(2, 1), (3, 2)]
        );
    }

    #[test]
    fn a_holder_that_takes_the_snapshot_ends_up_with_what_the_book_holds() {
        let mut book = certified_book(0);
        book.publish(policy("// v1"), 1, 1, None).unwrap();
        book.publish(groups(), 1, 2, None).unwrap();
        let snapshot = book.snapshot().unwrap();
        let mut holder = AuthorityState::default();
        for update in &snapshot.updates {
            let payload = verify(update, &snapshot.public_key().unwrap()).unwrap();
            assert_eq!(holder.apply(payload, true).applied, Applied::Replaced);
        }
        assert_eq!(holder.versions().len(), 2);
    }
}
