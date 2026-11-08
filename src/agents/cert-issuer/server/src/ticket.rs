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

//! What a successor ticket admits (docs/architecture/core-restarts.md, "Step 5, the ticket"), a pure table: the
//! credentials agent's successor, bound to the pod the ticket names, once for each certificate it asks for (its client
//! certificate and its custodian's), until the ticket expires. The supervisor installs a ticket once the record holds it
//! (`RecordingCa::take_ticket`); the handler asks it of each token-only request of the credentials agent's.

use cert_issuer_common::CertType;
use cert_issuer_common::record::{IssuePath, RecordEntry, TicketEntry};

/// The types a ticket admits: what the credentials agent's successor asks cert-issuer for.
const ADMITTED: [CertType; 2] = [CertType::Client, CertType::Custody];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Live {
    instance_binding: String,
    expires_at: i64,
    used: Vec<CertType>,
}

/// The ticket in force, if any: the last one taken.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tickets {
    live: Option<Live>,
}

impl Tickets {
    /// The table as the record leaves it: its last ticket, with the types already signed against it.
    pub fn restored(tickets: &[TicketEntry], entries: &[RecordEntry]) -> Self {
        let Some(last) = tickets.last() else {
            return Self::default();
        };
        let used = entries
            .iter()
            .filter(|e| {
                e.seq > last.seq
                    && e.path == IssuePath::Ticketed
                    && e.instance_binding.as_deref() == Some(last.instance_binding.as_str())
            })
            .map(|e| e.cert_type.clone())
            .collect();
        Self {
            live: Some(Live {
                instance_binding: last.instance_binding.clone(),
                expires_at: last.expires_at,
                used,
            }),
        }
    }

    /// A ticket the record now holds: it replaces any earlier one.
    pub fn install(&mut self, ticket: &TicketEntry) {
        self.live = Some(Live {
            instance_binding: ticket.instance_binding.clone(),
            expires_at: ticket.expires_at,
            used: Vec::new(),
        });
    }

    /// Whether the credentials agent's token-only request, bound to `instance_binding`, for `cert_type`, at `now` (Unix
    /// seconds), is admitted against the ticket; if it is, that type is used.
    pub fn admit(
        &mut self,
        instance_binding: Option<&str>,
        cert_type: &CertType,
        now: i64,
    ) -> bool {
        let Some(live) = self.live.as_mut() else {
            return false;
        };
        let admitted = now < live.expires_at
            && instance_binding == Some(live.instance_binding.as_str())
            && ADMITTED.contains(cert_type)
            && !live.used.contains(cert_type);
        if admitted {
            live.used.push(cert_type.clone());
        }
        admitted
    }

    /// A signing admitted against the ticket that then failed: its type may be asked for again.
    pub fn give_back(&mut self, cert_type: &CertType) {
        if let Some(live) = self.live.as_mut() {
            live.used.retain(|t| t != cert_type);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ticket(seq: u64, binding: &str, taken_at: i64) -> TicketEntry {
        TicketEntry {
            log: "l".into(),
            seq,
            identity: "credentials-agent.polar.serviceaccount.cluster.local".into(),
            instance_binding: binding.into(),
            taken_at,
            expires_at: taken_at + cert_issuer_common::ticket::TICKET_LIFETIME_SECS,
            issued_at: taken_at,
        }
    }

    fn signed(seq: u64, binding: &str, cert_type: CertType, path: IssuePath) -> RecordEntry {
        RecordEntry {
            log: "l".into(),
            seq,
            serial: format!("{seq:02x}"),
            identity: "credentials-agent.polar.serviceaccount.cluster.local".into(),
            cert_type,
            path,
            instance_binding: Some(binding.into()),
            signed_at: 0,
            not_after: 0,
            certificate_pem: String::new(),
        }
    }

    #[test]
    fn a_ticket_admits_its_pod_once_for_each_type_until_it_expires() {
        let mut t = Tickets::default();
        assert!(
            !t.admit(Some("pod"), &CertType::Client, 0),
            "no ticket, nothing"
        );
        t.install(&ticket(1, "pod", 1000));
        assert!(!t.admit(Some("another-pod"), &CertType::Client, 1001));
        assert!(!t.admit(None, &CertType::Client, 1001));
        assert!(
            !t.admit(Some("pod"), &CertType::Server, 1001),
            "not a type the successor asks for"
        );
        assert!(t.admit(Some("pod"), &CertType::Client, 1001));
        assert!(!t.admit(Some("pod"), &CertType::Client, 1002), "once");
        t.give_back(&CertType::Client);
        assert!(
            t.admit(Some("pod"), &CertType::Client, 1003),
            "given back by a signing that failed"
        );
        assert!(!t.admit(Some("pod"), &CertType::Custody, 1300), "expired");
        assert!(t.admit(Some("pod"), &CertType::Custody, 1299));
    }

    #[test]
    fn the_record_restores_the_last_ticket_and_what_was_signed_against_it() {
        let tickets = [ticket(1, "old-pod", 100), ticket(3, "pod", 1000)];
        let entries = [
            signed(2, "old-pod", CertType::Client, IssuePath::Ticketed),
            signed(4, "pod", CertType::Client, IssuePath::Ticketed),
            signed(5, "pod", CertType::Custody, IssuePath::TokenOnly),
        ];
        let mut t = Tickets::restored(&tickets, &entries);
        assert!(
            !t.admit(Some("old-pod"), &CertType::Custody, 1001),
            "the earlier ticket is gone"
        );
        assert!(
            !t.admit(Some("pod"), &CertType::Client, 1001),
            "already signed against it"
        );
        assert!(
            t.admit(Some("pod"), &CertType::Custody, 1001),
            "signed, but not against it"
        );
        assert_eq!(Tickets::restored(&[], &entries), Tickets::default());
    }

    #[derive(Debug, Clone)]
    enum Event {
        /// A ticket taken, binding the record's pod (the agent's own, or another's after a replacement).
        Take {
            own: bool,
        },
        Ask {
            own: Option<bool>,
            cert_type: CertType,
        },
        GiveBack {
            cert_type: CertType,
        },
    }

    fn cert_type() -> impl Strategy<Value = CertType> {
        prop_oneof![
            Just(CertType::Client),
            Just(CertType::Custody),
            Just(CertType::Server),
            Just(CertType::Signing),
        ]
    }

    fn event() -> impl Strategy<Value = Event> {
        prop_oneof![
            any::<bool>().prop_map(|own| Event::Take { own }),
            (proptest::option::of(any::<bool>()), cert_type())
                .prop_map(|(own, cert_type)| Event::Ask { own, cert_type }),
            cert_type().prop_map(|cert_type| Event::GiveBack { cert_type }),
        ]
    }

    proptest! {
        /// For any sequence of tickets, asks and givings-back, an ask is admitted exactly when the last ticket is under
        /// five minutes old, bound to the ask's pod, and that type, one the successor asks for, has not been admitted
        /// against it since (or was given back).
        #[test]
        fn an_ask_is_admitted_exactly_as_the_rule_says(
            steps in prop::collection::vec((event(), 0i64..200), 0..40),
        ) {
            let mut t = Tickets::default();
            let mut now = 0;
            let mut model: Option<(bool, i64, Vec<CertType>)> = None;
            for (n, (e, gap)) in steps.into_iter().enumerate() {
                now += gap;
                match e {
                    Event::Take { own } => {
                        let binding = if own { "pod" } else { "other-pod" };
                        t.install(&ticket(n as u64 + 1, binding, now));
                        model = Some((own, now + 300, Vec::new()));
                    }
                    Event::Ask { own, cert_type } => {
                        let binding = own.map(|own| if own { "pod" } else { "other-pod" });
                        let expected = model.as_ref().is_some_and(|(bound_own, expires, used)| {
                            own == Some(*bound_own)
                                && now < *expires
                                && matches!(cert_type, CertType::Client | CertType::Custody)
                                && !used.contains(&cert_type)
                        });
                        prop_assert_eq!(t.admit(binding, &cert_type, now), expected);
                        if expected {
                            model.as_mut().unwrap().2.push(cert_type);
                        }
                    }
                    Event::GiveBack { cert_type } => {
                        t.give_back(&cert_type);
                        if let Some(m) = model.as_mut() {
                            m.2.retain(|c| c != &cert_type);
                        }
                    }
                }
            }
        }
    }
}
