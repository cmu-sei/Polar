/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

//! Generic request/reply correlation over Cassini's pub/sub-only
//! transport, per docs/architecture/cassini-request-reply-protocol.md.
//! Any future agent building a request/reply interaction over Cassini
//! should wrap its own payload type in `Envelope<T>` rather than
//! duplicating a `correlation_id` field per message type -- the same
//! "generic struct carrying cross-cutting metadata alongside a payload"
//! idiom already used by gitlab/common's `WithInstance<T>`.
//!
//! Scoped to `rkyv` only, matching `WithInstance<T>`'s own precedent --
//! this type's whole job is the Cassini wire-transport leg of a
//! request/reply exchange, not persistence, so it doesn't require
//! `T: Serialize + DeserializeOwned` the way a type meant for
//! `EventLog` would. A caller whose payload also needs to be logged
//! (e.g. via `event_log::EventLog`) should derive `serde` on that
//! payload type directly, the same way `CredentialsEvent` does.

use rkyv::{Archive, Deserialize, Serialize};

/// Pairs a `correlation_id` with an arbitrary payload for one leg of a
/// request/reply exchange over Cassini.
#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub correlation_id: String,
    pub payload: T,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trips_through_rkyv() {
        let original = Envelope {
            correlation_id: "abc-123".to_string(),
            payload: "hello".to_string(),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: Envelope<String> =
            rkyv::from_bytes::<Envelope<String>, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(restored.correlation_id, "abc-123");
        assert_eq!(restored.payload, "hello");
    }
}
