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

//! What the observer says about a repository after each attempt to observe it, and where it says it.
//!
//! One message per repository per attempt, on [`GIT_REPO_OBSERVATION_STATUS`]. The graph keeps the latest on the
//! repository's node (the git processor), so a person can see which repositories are observed, which are waiting
//! and which are failing, and why, without reading a pod's log.

use polar::graph::nodes::git::RepoId;
use rkyv::{Archive, Deserialize, Serialize};

pub const GIT_REPO_OBSERVATION_STATUS: &str = "polar.git.observation.status";

/// The longest a reason is allowed to be, in characters. A status is for a person to read, and an error from a
/// remote can be anything.
pub const MAX_REASON_CHARS: usize = 300;

#[derive(Serialize, Deserialize, Archive, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationState {
    /// The last attempt fetched the repository and published what was new.
    Observing,
    /// The repository names a credential the observer does not have yet. It is not failing; it is waiting, and
    /// tries again when the credentials change and on its own frequency.
    PendingCredential,
    /// The last attempt could not go ahead, and `reason` says why: the credential cannot be used for this host,
    /// the clone failed, or the fetch failed.
    Failing,
}

impl ObservationState {
    /// The word written to the graph. Stable: queries and people depend on it.
    pub fn as_str(&self) -> &'static str {
        match self {
            ObservationState::Observing => "observing",
            ObservationState::PendingCredential => "pending-credential",
            ObservationState::Failing => "failing",
        }
    }
}

#[derive(Serialize, Deserialize, Archive, Debug, Clone, PartialEq)]
pub struct ObservationStatus {
    pub repo_id: RepoId,
    pub state: ObservationState,
    /// Why the last attempt could not go ahead. Never a credential, never more than [`MAX_REASON_CHARS`].
    pub reason: Option<String>,
    /// When a fetch last succeeded, in milliseconds since the epoch. `None` until this observer has had one: a
    /// replaced pod does not know when its predecessor's was.
    pub last_success: Option<i64>,
    /// Where each observed ref points, by ref name, sorted.
    pub tips: Vec<(String, String)>,
    /// Attempts in a row that could not go ahead. Zero when the last one did, or when the repository is only
    /// waiting for its credential.
    pub consecutive_failures: u32,
    /// When this status was made, in milliseconds since the epoch.
    pub observed_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> ObservationStatus {
        ObservationStatus {
            repo_id: RepoId::new("https_github.com_o_r".into()),
            state: ObservationState::Failing,
            reason: Some("could not be cloned".into()),
            last_success: Some(1_700_000_000_000),
            tips: vec![("refs/remotes/origin/main".into(), "abc".into())],
            consecutive_failures: 3,
            observed_at: 1_700_000_900_000,
        }
    }

    #[test]
    fn a_status_survives_the_wire() {
        let sent = status();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&sent).unwrap();
        let received = rkyv::from_bytes::<ObservationStatus, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(received, sent);
    }

    #[test]
    fn bytes_that_are_not_a_status_are_refused() {
        assert!(
            rkyv::from_bytes::<ObservationStatus, rkyv::rancor::Error>(b"not a status").is_err()
        );
    }

    #[test]
    fn each_state_has_its_own_word() {
        let words: std::collections::HashSet<_> = [
            ObservationState::Observing,
            ObservationState::PendingCredential,
            ObservationState::Failing,
        ]
        .iter()
        .map(|s| s.as_str())
        .collect();
        assert_eq!(words.len(), 3);
        assert_eq!(
            ObservationState::PendingCredential.as_str(),
            "pending-credential"
        );
    }
}
