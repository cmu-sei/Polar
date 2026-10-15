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

//! What the Polar git server says to the rest of Polar, and the topics it says it on.
//! Design: `docs/architecture/git-server.md`.

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// Published after a push has been accepted, once per ref that moved. The scheduler's observer
/// subscribes, so against the managed server it need not poll.
pub const POLAR_GIT_REPO_CHANGED_TOPIC: &str = "polar-git-repo-changed";

/// A ref of a hosted repository now points somewhere new. `pusher` is the identity from the
/// client's verified certificate.
#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub struct RepoChangedEvent {
    pub repo: String,
    pub refname: String,
    pub old: String,
    pub new: String,
    pub pusher: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_event_survives_the_wire() {
        let event = RepoChangedEvent {
            repo: "polar-schedules".into(),
            refname: "refs/heads/main".into(),
            old: "a".repeat(40),
            new: "b".repeat(40),
            pusher: "alice".into(),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&event).unwrap();
        let back = rkyv::from_bytes::<RepoChangedEvent, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(back, event);
    }
}
