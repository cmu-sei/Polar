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

//! The requests cert-issuer holds (docs/architecture/core-restarts.md, "3 in detail"): a bootstrap service's
//! token-only request that the floor admits and the floor state does not. Each is kept until the credentials agent
//! answers it, and that answer is what the next ask of the same request gets: signed if vouched for, refused if
//! refused. A pure table: the handler asks it, the supervisor and the mTLS routes give it the credentials agent's
//! answers.
//!
//! - The same request asked again is the same held entry: keyed by workload, pod, type and CSR.
//! - At most [`MAX_HELD`] at once, and each forgotten [`HELD_FOR`] after it was last asked for, so a flood of asks, or
//!   a request no one comes back for, cannot grow it.
//! - An answer is final: the first one for an entry stands, and an answer to an entry it does not hold changes nothing.

use cert_issuer_common::CertType;
use cert_issuer_common::held::{HeldDecision, HeldOffer};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The most requests held at once.
pub const MAX_HELD: usize = 64;
/// How long a held request is kept after it was last asked for.
pub const HELD_FOR: Duration = Duration::from_secs(600);

/// What makes two asks the same request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HeldKey {
    pub workload: String,
    pub instance_binding: Option<String>,
    pub cert_type: CertType,
    pub csr_pem: String,
}

/// What an ask of a held request comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    /// Held now for the first time: offer it to the credentials agent.
    New(HeldOffer),
    /// Held, not yet answered.
    Waiting(String),
    /// The credentials agent vouched for it: sign it.
    Vouched(String),
    /// The credentials agent refused it.
    Refused(String),
    /// Too many are held already.
    Full,
}

#[derive(Debug, Clone)]
enum Status {
    Waiting,
    Vouched,
    Refused(String),
}

#[derive(Debug, Clone)]
struct Held {
    offer: HeldOffer,
    status: Status,
    last_asked: Instant,
}

pub struct HeldRequests {
    held: HashMap<HeldKey, Held>,
    cap: usize,
    keep: Duration,
}

impl Default for HeldRequests {
    fn default() -> Self {
        Self::new(MAX_HELD, HELD_FOR)
    }
}

impl HeldRequests {
    pub fn new(cap: usize, keep: Duration) -> Self {
        Self {
            held: HashMap::new(),
            cap,
            keep,
        }
    }

    fn forget_old(&mut self, now: Instant) {
        let keep = self.keep;
        self.held
            .retain(|_, h| now.saturating_duration_since(h.last_asked) < keep);
    }

    /// An ask of the request `key`, which would be offered as `offer` (its `id` is set here).
    pub fn ask(&mut self, key: HeldKey, mut offer: HeldOffer, now: Instant) -> Asked {
        self.forget_old(now);
        if let Some(held) = self.held.get_mut(&key) {
            held.last_asked = now;
            let id = held.offer.id.clone();
            return match &held.status {
                Status::Waiting => Asked::Waiting(id),
                Status::Vouched => Asked::Vouched(id),
                Status::Refused(reason) => Asked::Refused(reason.clone()),
            };
        }
        if self.held.len() >= self.cap {
            return Asked::Full;
        }
        offer.id = uuid::Uuid::new_v4().to_string();
        self.held.insert(
            key,
            Held {
                offer: offer.clone(),
                status: Status::Waiting,
                last_asked: now,
            },
        );
        Asked::New(offer)
    }

    /// The credentials agent's answer to `id`. True if it was taken: an entry held and not yet answered.
    pub fn answer(&mut self, id: &str, decision: &HeldDecision) -> bool {
        let Some(held) = self
            .held
            .values_mut()
            .find(|h| h.offer.id == id && matches!(h.status, Status::Waiting))
        else {
            return false;
        };
        held.status = match decision {
            HeldDecision::Vouch => Status::Vouched,
            HeldDecision::Refuse { reason } => Status::Refused(reason.clone()),
        };
        true
    }

    /// The request `key` was signed: it is held no more.
    pub fn signed(&mut self, key: &HeldKey) {
        self.held.remove(key);
    }

    /// The held requests not yet answered, to offer again.
    pub fn waiting(&mut self, now: Instant) -> Vec<HeldOffer> {
        self.forget_old(now);
        let mut offers: Vec<HeldOffer> = self
            .held
            .values()
            .filter(|h| matches!(h.status, Status::Waiting))
            .map(|h| h.offer.clone())
            .collect();
        offers.sort_by(|a, b| a.id.cmp(&b.id));
        offers
    }

    pub fn len(&self) -> usize {
        self.held.len()
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    fn key(n: u8) -> HeldKey {
        HeldKey {
            workload: "system:serviceaccount:polar:cassini".to_string(),
            instance_binding: Some(format!("pod-{n}")),
            cert_type: CertType::Server,
            csr_pem: format!("csr-{n}"),
        }
    }

    fn offer(n: u8) -> HeldOffer {
        HeldOffer {
            id: String::new(),
            workload: "system:serviceaccount:polar:cassini".to_string(),
            identity: "cassini.polar.serviceaccount.cluster.local".to_string(),
            instance_binding: Some(format!("pod-{n}")),
            cert_type: CertType::Server,
            names: vec![],
            csr_pem: format!("csr-{n}"),
        }
    }

    fn new_id(asked: Asked) -> String {
        match asked {
            Asked::New(offer) => offer.id,
            other => panic!("expected a new hold, got {other:?}"),
        }
    }

    #[test]
    fn a_request_is_held_then_signed_once_the_credentials_agent_vouches() {
        let t = Instant::now();
        let mut held = HeldRequests::default();
        let id = new_id(held.ask(key(1), offer(1), t));
        assert_eq!(
            held.ask(key(1), offer(1), t + S),
            Asked::Waiting(id.clone())
        );
        assert_eq!(held.waiting(t + S).len(), 1);
        assert!(held.answer(&id, &HeldDecision::Vouch));
        assert_eq!(held.ask(key(1), offer(1), t + 2 * S), Asked::Vouched(id));
        assert!(
            held.waiting(t + 2 * S).is_empty(),
            "answered: not offered again"
        );
        held.signed(&key(1));
        assert!(held.is_empty());
    }

    #[test]
    fn a_refusal_is_what_every_later_ask_gets() {
        let t = Instant::now();
        let mut held = HeldRequests::default();
        let id = new_id(held.ask(key(1), offer(1), t));
        let refuse = HeldDecision::Refuse {
            reason: "already holds one".to_string(),
        };
        assert!(held.answer(&id, &refuse));
        assert!(
            !held.answer(&id, &HeldDecision::Vouch),
            "the first answer stands"
        );
        for later in 1..4 {
            assert_eq!(
                held.ask(key(1), offer(1), t + later * S),
                Asked::Refused("already holds one".to_string())
            );
        }
    }

    #[test]
    fn an_answer_for_a_request_not_held_changes_nothing() {
        let t = Instant::now();
        let mut held = HeldRequests::default();
        let id = new_id(held.ask(key(1), offer(1), t));
        assert!(!held.answer("not-held", &HeldDecision::Vouch));
        assert_eq!(held.ask(key(1), offer(1), t), Asked::Waiting(id));
    }

    #[test]
    fn a_full_table_holds_no_more_and_one_no_one_asks_for_is_forgotten() {
        let t = Instant::now();
        let mut held = HeldRequests::new(2, 600 * S);
        new_id(held.ask(key(1), offer(1), t));
        new_id(held.ask(key(2), offer(2), t + 100 * S));
        assert_eq!(held.ask(key(3), offer(3), t + 200 * S), Asked::Full);
        // The first, not asked for since t, is forgotten 600 s on; the second is still held.
        new_id(held.ask(key(3), offer(3), t + 600 * S));
        assert_eq!(held.len(), 2);
        assert!(matches!(
            held.ask(key(2), offer(2), t + 650 * S),
            Asked::Waiting(_)
        ));
        assert!(matches!(
            held.ask(key(1), offer(1), t + 650 * S),
            Asked::Full
        ));
    }

    #[test]
    fn a_request_asked_for_again_is_kept_from_its_last_ask() {
        let t = Instant::now();
        let mut held = HeldRequests::new(8, 600 * S);
        let id = new_id(held.ask(key(1), offer(1), t));
        assert_eq!(
            held.ask(key(1), offer(1), t + 500 * S),
            Asked::Waiting(id.clone())
        );
        // 900 s after it was first held, but only 400 after it was last asked for.
        assert_eq!(held.ask(key(1), offer(1), t + 900 * S), Asked::Waiting(id));
    }

    #[test]
    fn a_different_csr_or_pod_is_a_different_request() {
        let t = Instant::now();
        let mut held = HeldRequests::default();
        let a = new_id(held.ask(key(1), offer(1), t));
        let mut other_csr = key(1);
        other_csr.csr_pem = "another".to_string();
        let b = new_id(held.ask(other_csr, offer(1), t));
        assert_ne!(a, b);
        assert!(held.answer(&a, &HeldDecision::Vouch));
        let mut other_pod = key(1);
        other_pod.instance_binding = Some("pod-9".to_string());
        assert!(
            matches!(held.ask(other_pod, offer(1), t), Asked::New(_)),
            "a vouch is for its pod alone"
        );
    }
}
