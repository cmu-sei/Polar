//! What the Polar git server says to the rest of Polar, and the topics it says it on.
//! Design: `docs/architecture/git-server.md`.

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// Published after a push has been accepted, once per ref that moved. The scheduler's observer
/// subscribes, so against the managed server it need not poll.
pub const POLAR_GIT_REPO_CHANGED_TOPIC: &str = "polar-git-repo-changed";

/// A ref of a hosted repository now points somewhere new. `pusher` is the identity from the
/// client's verified certificate.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize)]
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
