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

use crate::state::CredentialsState;
use credentials_agent_common::issuance::WorkloadCertType;
use std::collections::HashMap;

/// How often a pod may be issued a certificate of one type: a restart in place is not this frequent, a loop or a flood
/// is (`deployments.md`, item 2).
pub const RENEWAL_BOUND_MILLIS: i64 = 5 * 60 * 1000;

/// The asks admitted that the ledger may not have recorded yet: an issuance in flight, or one vouched for that
/// cert-issuer's record has not reported. Held in memory by the ledger actor, which answers one ask at a time; lost at
/// a restart, which only lifts a bound, since what was issued is in the ledger.
#[derive(Debug, Default)]
pub struct Admissions {
    admitted: HashMap<(String, String, WorkloadCertType), i64>,
}

impl Admissions {
    /// Whether `identity`'s pod `instance_binding` may be issued a certificate of `cert_type` at `now`: not if it was
    /// issued one, or admitted to be, less than the bound before. Admitting records `now`.
    pub fn admit(
        &mut self,
        state: &CredentialsState,
        identity: &str,
        instance_binding: &str,
        cert_type: WorkloadCertType,
        now: i64,
    ) -> bool {
        let key = (
            identity.to_string(),
            instance_binding.to_string(),
            cert_type,
        );
        let recent = |at: Option<i64>| at.is_some_and(|at| now - at < RENEWAL_BOUND_MILLIS);
        if recent(self.admitted.get(&key).copied())
            || recent(state.last_issued(identity, instance_binding, cert_type))
        {
            return false;
        }
        self.admitted.insert(key, now);
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
        if self.admitted.get(&key) == Some(&at) {
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

    #[test]
    fn a_pod_is_admitted_once_within_the_bound_for_each_type() {
        let state = CredentialsState::new();
        let mut admissions = Admissions::default();
        assert!(admissions.admit(&state, ID, "pod-1", Client, 0));
        assert!(!admissions.admit(&state, ID, "pod-1", Client, 4 * MINUTE));
        assert!(
            admissions.admit(&state, ID, "pod-1", Server, 4 * MINUTE),
            "per type"
        );
        assert!(
            admissions.admit(&state, ID, "pod-2", Client, 4 * MINUTE),
            "per pod"
        );
        assert!(
            admissions.admit(&state, ID, "pod-1", Client, 5 * MINUTE),
            "once the bound has passed"
        );
    }

    #[test]
    fn an_issuance_the_ledger_recorded_holds_the_bound_after_a_restart() {
        let mut state = CredentialsState::new();
        state.apply(&issued("pod-1", Client, "s1", 10 * MINUTE));
        // A restarted credentials agent remembers no admissions, only the ledger.
        let mut admissions = Admissions::default();
        assert!(!admissions.admit(&state, ID, "pod-1", Client, 12 * MINUTE));
        assert!(admissions.admit(&state, ID, "pod-1", Client, 15 * MINUTE));
    }

    #[test]
    fn a_released_admission_lifts_the_bound_and_only_its_own() {
        let state = CredentialsState::new();
        let mut admissions = Admissions::default();
        assert!(admissions.admit(&state, ID, "pod-1", Client, 0));
        // Released for another time: not this admission, which stands.
        admissions.release(ID, "pod-1", Client, 7);
        assert!(!admissions.admit(&state, ID, "pod-1", Client, MINUTE));
        admissions.release(ID, "pod-1", Client, 0);
        assert!(admissions.admit(&state, ID, "pod-1", Client, MINUTE));
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
                after: i64,
            },
            Issue {
                pod: u8,
                server: bool,
                after: i64,
            },
            /// The last admission of this pod and type given back, as a failed signing gives it back.
            Release {
                pod: u8,
                server: bool,
            },
            /// The credentials agent restarted: its admissions are gone, the ledger is not.
            Restart {
                after: i64,
            },
        }

        fn op() -> impl Strategy<Value = Op> {
            let pod = 0u8..3;
            // Exactly the bound, now and then, where the rule's edge is.
            let after = prop_oneof![3 => 0i64..(4 * MINUTE), 1 => Just(RENEWAL_BOUND_MILLIS)];
            prop_oneof![
                4 => (pod.clone(), any::<bool>(), after.clone())
                    .prop_map(|(pod, server, after)| Op::Ask { pod, server, after }),
                2 => (pod.clone(), any::<bool>(), after.clone())
                    .prop_map(|(pod, server, after)| Op::Issue { pod, server, after }),
                1 => (pod, any::<bool>()).prop_map(|(pod, server)| Op::Release { pod, server }),
                1 => after.prop_map(|after| Op::Restart { after }),
            ]
        }

        fn cert_type(server: bool) -> WorkloadCertType {
            if server { Server } else { Client }
        }

        proptest! {
            /// Every ask is admitted exactly when that pod had no admission (since the last restart, and not given
            /// back) and no recorded issuance of that type in the bound before it.
            #[test]
            fn an_ask_is_admitted_exactly_when_nothing_of_its_kind_came_within_the_bound(
                ops in prop::collection::vec(op(), 1..60)
            ) {
                let mut state = CredentialsState::new();
                let mut admissions = Admissions::default();
                let mut admitted: HashMap<(u8, bool), i64> = HashMap::new();
                let mut issued_at: HashMap<(u8, bool), i64> = HashMap::new();
                let mut now = 0;
                for (n, op) in ops.into_iter().enumerate() {
                    match op {
                        Op::Ask { pod, server, after } => {
                            now += after;
                            let recent = |at: Option<&i64>| at.is_some_and(|at| now - at < RENEWAL_BOUND_MILLIS);
                            let expected = !recent(admitted.get(&(pod, server)))
                                && !recent(issued_at.get(&(pod, server)));
                            let answer = admissions.admit(&state, ID, &format!("pod-{pod}"), cert_type(server), now);
                            prop_assert_eq!(answer, expected, "at {} ({:?})", now, (pod, server));
                            if answer {
                                admitted.insert((pod, server), now);
                            }
                        }
                        Op::Issue { pod, server, after } => {
                            now += after;
                            state.apply(&issued(&format!("pod-{pod}"), cert_type(server), &format!("s{n}"), now));
                            issued_at.insert((pod, server), now);
                        }
                        Op::Release { pod, server } => {
                            if let Some(at) = admitted.remove(&(pod, server)) {
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
