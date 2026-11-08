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

//! The successor ticket (docs/architecture/core-restarts.md, "Step 5, the ticket"): as its orderly ending begins, the
//! credentials agent tells cert-issuer that its successor, the same pod restarted in place, may be signed on its token.
//! cert-issuer takes a ticket only when the broker says the credentials agent published it, and binds it to the pod its
//! own record gives the credentials agent's last client certificate: the ticket carries nothing that could name another.

use serde::{Deserialize, Serialize};

/// The credentials agent publishes its ticket on this topic, to cert-issuer.
pub const SUCCESSOR_TICKETS_TOPIC: &str = "cert-issuer-successor-tickets";

/// How long a ticket admits its successor's signings: the renewal bound (docs/architecture/deployments.md, item 2).
pub const TICKET_LIFETIME_SECS: i64 = 300;

/// The ticket, as the credentials agent publishes it: when, by its clock (Unix seconds), for the record. cert-issuer
/// reckons its five minutes from when it took it, by its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuccessorTicket {
    pub issued_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticket_reads_back_as_it_was_written_and_nothing_else_reads_as_one() {
        let ticket = SuccessorTicket {
            issued_at: 1_798_000_000,
        };
        let bytes = serde_json::to_vec(&ticket).unwrap();
        assert_eq!(
            serde_json::from_slice::<SuccessorTicket>(&bytes).unwrap(),
            ticket
        );
        assert!(
            serde_json::from_slice::<SuccessorTicket>(br#"{"issued_at":1,"pod":"other"}"#).is_err()
        );
        assert!(serde_json::from_slice::<SuccessorTicket>(br#"{}"#).is_err());
    }
}
