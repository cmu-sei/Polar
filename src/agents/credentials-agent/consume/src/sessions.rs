//! The sessions a workload seals its token to (credentials-agent-common's `workload_issuance`): short-lived, single
//! use, and kept only in memory, so a restart simply makes the workload open another.
//!
//! The table is bounded, because anyone who can reach the observer can open one: expired sessions are dropped
//! first, and when it is still full a new one is refused rather than an old one evicted, so a flood cannot push a
//! legitimate workload's session out from under it.

use credentials_agent_common::ephemeral_exchange::EphemeralSession;
use credentials_agent_common::workload_issuance::SessionOffer;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a session is kept. Long enough for a workload to seal and send, short enough that an abandoned one
/// goes quickly.
pub const SESSION_TTL: Duration = Duration::from_secs(60);
/// The most sessions open at once.
pub const MAX_OPEN_SESSIONS: usize = 256;

/// Too many sessions are open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy;

pub struct Sessions {
    open: HashMap<String, (Instant, EphemeralSession)>,
    ttl: Duration,
    cap: usize,
}

impl Sessions {
    pub fn new(ttl: Duration, cap: usize) -> Self {
        Self {
            open: HashMap::new(),
            ttl,
            cap,
        }
    }

    fn drop_expired(&mut self, now: Instant) {
        let ttl = self.ttl;
        self.open
            .retain(|_, (opened, _)| now.duration_since(*opened) < ttl);
    }

    /// Opens a session and returns what the workload needs to seal to it.
    pub fn open(&mut self, now: Instant) -> Result<SessionOffer, Busy> {
        self.drop_expired(now);
        if self.open.len() >= self.cap {
            return Err(Busy);
        }
        let session = EphemeralSession::open();
        let offer = SessionOffer {
            session_id: uuid::Uuid::new_v4().to_string(),
            public_key: session.public_key(),
            valid_for_secs: self.ttl.as_secs(),
        };
        self.open.insert(offer.session_id.clone(), (now, session));
        Ok(offer)
    }

    /// Takes the session out, for its one use. None if there is no such session, it was already used, or it has
    /// expired (and an expired one is dropped).
    pub fn take(&mut self, session_id: &str, now: Instant) -> Option<EphemeralSession> {
        let (opened, session) = self.open.remove(session_id)?;
        (now.duration_since(opened) < self.ttl).then_some(session)
    }

    pub fn len(&self) -> usize {
        self.open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = Duration::from_secs(60);

    fn table(cap: usize) -> Sessions {
        Sessions::new(TTL, cap)
    }

    #[test]
    fn an_opened_session_is_taken_once_and_only_once() {
        let mut sessions = table(8);
        let now = Instant::now();
        let offer = sessions.open(now).unwrap();
        assert_eq!(offer.valid_for_secs, 60);
        assert!(sessions.take(&offer.session_id, now).is_some());
        assert!(sessions.take(&offer.session_id, now).is_none(), "used up");
    }

    #[test]
    fn the_session_taken_is_the_one_the_offer_described() {
        let mut sessions = table(8);
        let now = Instant::now();
        let offer = sessions.open(now).unwrap();
        let session = sessions.take(&offer.session_id, now).unwrap();
        assert_eq!(session.public_key(), offer.public_key);
    }

    #[test]
    fn sessions_have_their_own_ids_and_keys() {
        let mut sessions = table(8);
        let now = Instant::now();
        let a = sessions.open(now).unwrap();
        let b = sessions.open(now).unwrap();
        assert_ne!(a.session_id, b.session_id);
        assert_ne!(a.public_key, b.public_key);
    }

    #[test]
    fn a_session_nobody_opened_is_not_there() {
        let mut sessions = table(8);
        assert!(sessions.take("never-opened", Instant::now()).is_none());
    }

    #[test]
    fn a_session_is_good_until_its_time_is_up_and_not_after() {
        let mut sessions = table(8);
        let now = Instant::now();
        let early = sessions.open(now).unwrap();
        let late = sessions.open(now).unwrap();
        assert!(
            sessions
                .take(&early.session_id, now + TTL - Duration::from_secs(1))
                .is_some()
        );
        assert!(
            sessions.take(&late.session_id, now + TTL).is_none(),
            "at the deadline it is gone"
        );
        assert!(
            sessions.is_empty(),
            "taking an expired session still removes it"
        );
    }

    #[test]
    fn a_full_table_refuses_a_new_session_and_keeps_the_ones_it_has() {
        let mut sessions = table(2);
        let now = Instant::now();
        let first = sessions.open(now).unwrap();
        sessions.open(now).unwrap();
        assert_eq!(sessions.open(now), Err(Busy));
        assert_eq!(sessions.len(), 2);
        assert!(
            sessions.take(&first.session_id, now).is_some(),
            "not evicted by the flood"
        );
    }

    #[test]
    fn expired_sessions_make_room() {
        let mut sessions = table(2);
        let now = Instant::now();
        sessions.open(now).unwrap();
        sessions.open(now).unwrap();
        assert!(
            sessions.open(now + TTL).is_ok(),
            "the two old ones have expired"
        );
        assert_eq!(sessions.len(), 1);
    }
}
