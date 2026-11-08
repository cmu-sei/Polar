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

//! Properties of the requests cert-issuer holds (docs/architecture/core-restarts.md, "3 in detail"): for any sequence of
//! asks, answers and signings, never more held than the cap; the same request asked again is the same entry; and the
//! first answer to an entry is the one every later ask of it gets.

use cert_issuer::held::{Asked, HeldKey, HeldRequests};
use cert_issuer_common::CertType;
use cert_issuer_common::held::{HeldDecision, HeldOffer};
use proptest::prelude::*;
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
enum Step {
    /// Ask for request `n` after `gap` seconds.
    Ask { n: u8, gap: u64 },
    /// Answer the `pick`th id seen so far (or one never handed out, if there is none).
    Answer { pick: usize, vouch: bool },
    /// Request `n` was signed.
    Signed { n: u8 },
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => (0u8..6, 0u64..400).prop_map(|(n, gap)| Step::Ask { n, gap }),
        2 => (0usize..8, any::<bool>()).prop_map(|(pick, vouch)| Step::Answer { pick, vouch }),
        1 => (0u8..6).prop_map(|n| Step::Signed { n }),
    ]
}

fn key(n: u8) -> HeldKey {
    HeldKey {
        workload: "system:serviceaccount:polar:credentials-agent-observer".to_string(),
        instance_binding: Some(format!("pod-{}", n % 3)),
        cert_type: CertType::Client,
        csr_pem: format!("csr-{n}"),
    }
}

fn offer(n: u8) -> HeldOffer {
    HeldOffer {
        id: String::new(),
        workload: key(n).workload,
        identity: "credentials-agent-observer.polar.serviceaccount.cluster.local".to_string(),
        instance_binding: key(n).instance_binding,
        cert_type: CertType::Client,
        names: vec![],
        csr_pem: key(n).csr_pem,
    }
}

proptest! {
    #[test]
    fn the_table_stays_bounded_and_the_first_answer_to_a_request_stands(
        steps in prop::collection::vec(step(), 0..40),
        cap in 1usize..5,
    ) {
        let mut held = HeldRequests::new(cap, Duration::from_secs(600));
        let mut now = Instant::now();
        let mut ids: Vec<String> = Vec::new();
        // The first answer each id was given, and the id each request was last held under.
        let mut answered: HashMap<String, bool> = HashMap::new();
        let mut held_as: HashMap<u8, String> = HashMap::new();
        for step in steps {
            match step {
                Step::Ask { n, gap } => {
                    now += Duration::from_secs(gap);
                    match held.ask(key(n), offer(n), now) {
                        Asked::New(o) => {
                            prop_assert!(!ids.contains(&o.id), "a new hold has a new id");
                            prop_assert_eq!(&o.csr_pem, &key(n).csr_pem);
                            ids.push(o.id.clone());
                            held_as.insert(n, o.id);
                        }
                        Asked::Waiting(id) => {
                            prop_assert_eq!(Some(&id), held_as.get(&n), "the same request is the same entry");
                            prop_assert!(!answered.contains_key(&id), "an answered request is not waiting");
                        }
                        Asked::Vouched(id) => {
                            prop_assert_eq!(Some(&id), held_as.get(&n));
                            prop_assert_eq!(answered.get(&id), Some(&true), "vouched only by its first answer");
                        }
                        Asked::Refused(_) => {
                            let id = held_as.get(&n).cloned();
                            prop_assert_eq!(id.as_ref().and_then(|id| answered.get(id)), Some(&false));
                        }
                        Asked::Full => prop_assert_eq!(held.len(), cap),
                    }
                }
                Step::Answer { pick, vouch } => {
                    let id = ids.get(pick).cloned().unwrap_or_else(|| "never-handed-out".to_string());
                    let decision = if vouch {
                        HeldDecision::Vouch
                    } else {
                        HeldDecision::Refuse { reason: "no".to_string() }
                    };
                    let taken = held.answer(&id, &decision);
                    if taken {
                        prop_assert!(!answered.contains_key(&id), "an entry is answered once");
                        answered.insert(id, vouch);
                    }
                }
                Step::Signed { n } => {
                    held.signed(&key(n));
                }
            }
            prop_assert!(held.len() <= cap, "never more held than the cap");
            for waiting in held.waiting(now) {
                prop_assert!(!answered.contains_key(&waiting.id), "only the unanswered are offered again");
            }
        }
    }
}
