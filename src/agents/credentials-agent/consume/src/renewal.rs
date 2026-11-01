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

//! The renewal rule (docs/architecture/deployments.md, "The renewal rule"): a pod restarted in place keeps its UID and
//! asks again, and is issued again, but not more than one certificate of a type in [`RENEWAL_BOUND_MILLIS`].
//!
//! Keyed on the request itself (docs/architecture/deployments.md, "A retry is not a second request"): a request asked
//! again, its reply lost, asks for the same key, and is admitted again within the bound; a request for another key is
//! a second request, and is not.

use crate::state::CredentialsState;
use credentials_agent_common::issuance::WorkloadCertType;
use std::collections::HashMap;

/// How often a pod may be issued a certificate of one type: a restart in place is not this frequent, a loop or a flood
/// is (`deployments.md`, item 2).
pub const RENEWAL_BOUND_MILLIS: i64 = 5 * 60 * 1000;

/// The asks admitted that the ledger may not have recorded yet: an issuance in flight, or one vouched for that
/// cert-issuer's record has not reported. Held in memory by the ledger actor, which answers one ask at a time; lost at
/// a restart, which only lifts a bound, since what was issued is in the ledger.
/// An identity's pod and a certificate type: what one admission is of.
type Admitted = (String, String, WorkloadCertType);

/// When an admission was made, and for which key (its SubjectPublicKeyInfo; none when the request's CSR could not be
/// read).
type Admission = (i64, Option<Vec<u8>>);

#[derive(Debug, Default)]
pub struct Admissions {
    /// The last admission of each pod and type.
    admitted: HashMap<Admitted, Admission>,
}

/// Whether `a` and `b` are one key: both read, and equal. A key that could not be read is no other key's.
fn same_key(a: Option<&[u8]>, b: Option<&[u8]>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a == b)
}

impl Admissions {
    /// Whether `identity`'s pod `instance_binding` may be issued a certificate of `cert_type` for `key` (the
    /// SubjectPublicKeyInfo its CSR asks for, `cert_issuer_common::public_key::csr_key`) at `now`: not if it was
    /// issued one, or admitted to be, less than the bound before, for another key. The same key within the bound is
    /// the same request asked again, and is admitted again. Admitting records `now` and the key.
    pub fn admit(
        &mut self,
        state: &CredentialsState,
        identity: &str,
        instance_binding: &str,
        cert_type: WorkloadCertType,
        key: Option<&[u8]>,
        now: i64,
    ) -> bool {
        let entry = (
            identity.to_string(),
            instance_binding.to_string(),
            cert_type,
        );
        let recent = |at: i64| now - at < RENEWAL_BOUND_MILLIS;
        let admitted_another = self
            .admitted
            .get(&entry)
            .is_some_and(|(at, admitted)| recent(*at) && !same_key(admitted.as_deref(), key));
        let issued_another = state
            .last_issued(identity, instance_binding, cert_type)
            .is_some_and(|at| {
                recent(at)
                    && !same_key(
                        state
                            .last_issued_key(identity, instance_binding, cert_type)
                            .as_deref(),
                        key,
                    )
            });
        if admitted_another || issued_another {
            return false;
        }
        self.admitted.insert(entry, (now, key.map(<[u8]>::to_vec)));
        true
    }

    /// Gives back the admission made at `at`, for an issuance that did not happen. A later admission stands.
    pub fn release(
        &mut self,
        identity: &str,
        instance_binding: &str,
        cert_type: WorkloadCertType,
        at: i64,
    ) {
        let key = (
            identity.to_string(),
            instance_binding.to_string(),
            cert_type,
        );
        if self
            .admitted
            .get(&key)
            .is_some_and(|(admitted, _)| *admitted == at)
        {
            self.admitted.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::CredentialsState;
    use credentials_agent_common::events::CredentialsEvent;
    use credentials_agent_common::issuance::WorkloadCertType::{self, Client, Server};

    const ID: &str = "kube-observer.polar.serviceaccount.cluster.local";
    const MINUTE: i64 = 60_000;

    fn issued(pod: &str, cert_type: WorkloadCertType, serial: &str, at: i64) -> CredentialsEvent {
        CredentialsEvent::IssuanceCompleted {
            identity: ID.to_string(),
            instance_binding: pod.to_string(),
            serial: serial.to_string(),
            timestamp: at,
            cert_type,
            grant_key: None,
            certificate_pem: "pem".to_string(),
        }
    }

    /// A key, its CSR's key as the admission reads it, and a certificate of it as the ledger records one.
    struct Key {
        asked: Vec<u8>,
        certificate_pem: String,
    }

    fn key() -> Key {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        let csr = rcgen::CertificateParams::new(vec![ID.to_string()])
            .unwrap()
            .serialize_request(&key)
            .unwrap()
            .pem()
            .unwrap();
        Key {
            asked: cert_issuer_common::public_key::csr_key(&csr).unwrap(),
            certificate_pem: rcgen::CertificateParams::new(vec![ID.to_string()])
                .unwrap()
                .self_signed(&key)
                .unwrap()
                .pem(),
        }
    }

    fn issued_for(key: &Key, pod: &str, cert_type: WorkloadCertType, at: i64) -> CredentialsEvent {
        CredentialsEvent::IssuanceCompleted {
            identity: ID.to_string(),
            instance_binding: pod.to_string(),
            serial: format!("s{at}"),
            timestamp: at,
            cert_type,
            grant_key: None,
            certificate_pem: key.certificate_pem.clone(),
        }
    }

    // docs/architecture/deployments.md, "A retry is not a second request": a request asked again, its reply lost, is
    // admitted again within the bound; one for another key is not.
    #[test]
    fn the_same_key_asked_again_within_the_bound_is_admitted_and_another_key_is_not() {
        let state = CredentialsState::new();
        let (a, b) = (key(), key());
        let mut admissions = Admissions::default();
        assert!(admissions.admit(&state, ID, "pod-1", Client, Some(&a.asked), 0));
        assert!(
            admissions.admit(&state, ID, "pod-1", Client, Some(&a.asked), MINUTE),
            "asked again"
        );
        assert!(!admissions.admit(&state, ID, "pod-1", Client, Some(&b.asked), 2 * MINUTE));
        assert!(
            !admissions.admit(&state, ID, "pod-1", Client, None, 2 * MINUTE),
            "a key that cannot be read is another"
        );
        // Admitted again, the bound runs from then: another key waits for it.
        assert!(!admissions.admit(&state, ID, "pod-1", Client, Some(&b.asked), 5 * MINUTE));
        assert!(admissions.admit(&state, ID, "pod-1", Client, Some(&b.asked), 6 * MINUTE));
    }

    #[test]
    fn the_key_an_issuance_was_recorded_for_is_admitted_again_after_a_restart_and_another_is_not() {
        let mut state = CredentialsState::new();
        let (a, b) = (key(), key());
        state.apply(&issued_for(&a, "pod-1", Client, 10 * MINUTE));
        // A restarted credentials agent remembers no admissions: the ledger's record says which key was issued.
        let mut admissions = Admissions::default();
        assert!(admissions.admit(&state, ID, "pod-1", Client, Some(&a.asked), 11 * MINUTE));
        let mut admissions = Admissions::default();
        assert!(!admissions.admit(&state, ID, "pod-1", Client, Some(&b.asked), 11 * MINUTE));
        assert!(
            admissions.admit(&state, ID, "pod-1", Server, Some(&b.asked), 11 * MINUTE),
            "per type"
        );
    }

    #[test]
    fn a_pod_is_admitted_once_within_the_bound_for_each_type() {
        let state = CredentialsState::new();
        let mut admissions = Admissions::default();
        assert!(admissions.admit(&state, ID, "pod-1", Client, None, 0));
        assert!(!admissions.admit(&state, ID, "pod-1", Client, None, 4 * MINUTE));
        assert!(
            admissions.admit(&state, ID, "pod-1", Server, None, 4 * MINUTE),
            "per type"
        );
        assert!(
            admissions.admit(&state, ID, "pod-2", Client, None, 4 * MINUTE),
            "per pod"
        );
        assert!(
            admissions.admit(&state, ID, "pod-1", Client, None, 5 * MINUTE),
            "once the bound has passed"
        );
    }

    #[test]
    fn an_issuance_the_ledger_recorded_holds_the_bound_after_a_restart() {
        let mut state = CredentialsState::new();
        state.apply(&issued("pod-1", Client, "s1", 10 * MINUTE));
        // A restarted credentials agent remembers no admissions, only the ledger.
        let mut admissions = Admissions::default();
        assert!(!admissions.admit(&state, ID, "pod-1", Client, None, 12 * MINUTE));
        assert!(admissions.admit(&state, ID, "pod-1", Client, None, 15 * MINUTE));
    }

    #[test]
    fn a_released_admission_lifts_the_bound_and_only_its_own() {
        let state = CredentialsState::new();
        let mut admissions = Admissions::default();
        assert!(admissions.admit(&state, ID, "pod-1", Client, None, 0));
        // Released for another time: not this admission, which stands.
        admissions.release(ID, "pod-1", Client, 7);
        assert!(!admissions.admit(&state, ID, "pod-1", Client, None, MINUTE));
        admissions.release(ID, "pod-1", Client, 0);
        assert!(admissions.admit(&state, ID, "pod-1", Client, None, MINUTE));
    }

    mod props {
        use super::*;
        use proptest::prelude::*;
        use std::collections::HashMap;

        #[derive(Debug, Clone)]
        enum Op {
            Ask {
                pod: u8,
                server: bool,
                key: u8,
                after: i64,
            },
            Issue {
                pod: u8,
                server: bool,
                key: u8,
                after: i64,
            },
            /// The last admission of this pod and type given back, as a failed signing gives it back.
            Release { pod: u8, server: bool },
            /// The credentials agent restarted: its admissions are gone, the ledger is not.
            Restart { after: i64 },
        }

        /// The keys a request asks for: two keys, and a CSR or a certificate that cannot be read (the last).
        const UNREADABLE: u8 = 2;

        fn keys() -> &'static [Key] {
            static KEYS: std::sync::OnceLock<Vec<Key>> = std::sync::OnceLock::new();
            KEYS.get_or_init(|| {
                vec![
                    key(),
                    key(),
                    Key {
                        asked: Vec::new(),
                        certificate_pem: "pem".to_string(),
                    },
                ]
            })
        }

        fn asked(key: u8) -> Option<&'static [u8]> {
            (key != UNREADABLE).then(|| keys()[key as usize].asked.as_slice())
        }

        fn op() -> impl Strategy<Value = Op> {
            let pod = 0u8..3;
            let key = 0u8..=UNREADABLE;
            // Exactly the bound, now and then, where the rule's edge is.
            let after = prop_oneof![3 => 0i64..(4 * MINUTE), 1 => Just(RENEWAL_BOUND_MILLIS)];
            prop_oneof![
                4 => (pod.clone(), any::<bool>(), key.clone(), after.clone())
                    .prop_map(|(pod, server, key, after)| Op::Ask { pod, server, key, after }),
                2 => (pod.clone(), any::<bool>(), key, after.clone())
                    .prop_map(|(pod, server, key, after)| Op::Issue { pod, server, key, after }),
                1 => (pod, any::<bool>()).prop_map(|(pod, server)| Op::Release { pod, server }),
                1 => after.prop_map(|after| Op::Restart { after }),
            ]
        }

        fn cert_type(server: bool) -> WorkloadCertType {
            if server { Server } else { Client }
        }

        proptest! {
            /// Every ask is admitted exactly when that pod had no admission (since the last restart, and not given
            /// back) and no recorded issuance of that type in the bound before it, for another key: the same key
            /// within the bound is the same request asked again, and a key that cannot be read is never the same.
            #[test]
            fn an_ask_is_admitted_exactly_when_nothing_of_its_kind_came_within_the_bound(
                ops in prop::collection::vec(op(), 1..60)
            ) {
                let mut state = CredentialsState::new();
                let mut admissions = Admissions::default();
                let mut admitted: HashMap<(u8, bool), (i64, u8)> = HashMap::new();
                let mut issued_at: HashMap<(u8, bool), (i64, u8)> = HashMap::new();
                let mut now = 0;
                for (n, op) in ops.into_iter().enumerate() {
                    match op {
                        Op::Ask { pod, server, key, after } => {
                            now += after;
                            let another = |given: Option<&(i64, u8)>| {
                                given.is_some_and(|(at, given)| {
                                    now - at < RENEWAL_BOUND_MILLIS && (*given != key || key == UNREADABLE)
                                })
                            };
                            let expected = !another(admitted.get(&(pod, server)))
                                && !another(issued_at.get(&(pod, server)));
                            let answer = admissions.admit(
                                &state, ID, &format!("pod-{pod}"), cert_type(server), asked(key), now,
                            );
                            prop_assert_eq!(answer, expected, "at {} ({:?}, key {})", now, (pod, server), key);
                            if answer {
                                admitted.insert((pod, server), (now, key));
                            }
                        }
                        Op::Issue { pod, server, key, after } => {
                            now += after;
                            let mut event = issued_for(&keys()[key as usize], &format!("pod-{pod}"), cert_type(server), now);
                            if let CredentialsEvent::IssuanceCompleted { serial, .. } = &mut event {
                                *serial = format!("s{n}");
                            }
                            state.apply(&event);
                            issued_at.insert((pod, server), (now, key));
                        }
                        Op::Release { pod, server } => {
                            if let Some((at, _)) = admitted.remove(&(pod, server)) {
                                admissions.release(ID, &format!("pod-{pod}"), cert_type(server), at);
                            }
                        }
                        Op::Restart { after } => {
                            now += after;
                            admissions = Admissions::default();
                            admitted.clear();
                        }
                    }
                }
            }
        }
    }
}
