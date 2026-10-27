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

//! The floor's gate on token-only requests (docs/architecture/core-restarts.md, "3 in detail"): the floor state, the
//! requests held, and how a held request is offered to the credentials agent. The handler asks it of each token-only
//! request for a bootstrap identity; the supervisor tells it what cert-issuer hears and gives it the credentials
//! agent's answers over Cassini; the mTLS routes do the same while the credentials agent's Cassini session is lost.

use crate::floor_state::{FloorState, Heard, Open};
use crate::held::{Asked, HeldKey, HeldRequests};
use cert_issuer_common::held::{HeldOffer, HeldReply};
use std::sync::Mutex;
use std::time::Instant;
use tracing::info;

/// Sends held requests to the credentials agent over Cassini.
pub type Offer = Box<dyn Fn(Vec<HeldOffer>) + Send + Sync>;

pub struct FloorGate {
    state: Mutex<FloorState>,
    held: Mutex<HeldRequests>,
    offer: Offer,
}

impl FloorGate {
    pub fn new(state: FloorState, offer: Offer) -> Self {
        Self::with_held(state, HeldRequests::default(), offer)
    }

    pub fn with_held(state: FloorState, held: HeldRequests, offer: Offer) -> Self {
        Self {
            state: Mutex::new(state),
            held: Mutex::new(held),
            offer,
        }
    }

    pub fn open(&self, now: Instant) -> Open {
        self.state
            .lock()
            .map(|s| s.open(now))
            .unwrap_or(Open::Nothing)
    }

    /// Tells the state what was heard at `now`, logging what is open when that changes.
    pub fn hear(&self, heard: Heard, now: Instant) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let before = state.open(now);
        state.hear(heard, now);
        let after = state.open(now);
        if after != before {
            info!(?heard, from = ?before, to = ?after, "floor state changed");
        }
    }

    /// An ask of a held request; a request held now for the first time is offered to the credentials agent.
    pub fn ask(&self, key: HeldKey, offer: HeldOffer, now: Instant) -> Asked {
        let asked = match self.held.lock() {
            Ok(mut held) => held.ask(key, offer, now),
            Err(_) => return Asked::Full,
        };
        if let Asked::New(offer) = &asked {
            (self.offer)(vec![offer.clone()]);
        }
        asked
    }

    pub fn signed(&self, key: &HeldKey) {
        if let Ok(mut held) = self.held.lock() {
            held.signed(key);
        }
    }

    /// The credentials agent's answer; true if it was taken.
    pub fn answer(&self, reply: &HeldReply) -> bool {
        self.held
            .lock()
            .is_ok_and(|mut held| held.answer(&reply.id, &reply.decision))
    }

    /// The held requests not yet answered.
    pub fn waiting(&self, now: Instant) -> Vec<HeldOffer> {
        self.held
            .lock()
            .map(|mut held| held.waiting(now))
            .unwrap_or_default()
    }

    /// Offers every unanswered held request again (each time the credentials agent is heard, so one lost on the way
    /// is offered again).
    pub fn offer_waiting(&self, now: Instant) {
        let waiting = self.waiting(now);
        if !waiting.is_empty() {
            (self.offer)(waiting);
        }
    }
}
