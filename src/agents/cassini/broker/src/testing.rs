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

//! Hooks for the broker's own tests (the `testing` feature, never enabled in a build that ships): a point where a test
//! holds the broker, so that an order the scheduler otherwise decides is the same on every run.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// How long a Listener waits before it binds its connection to the session it was registered to, in milliseconds.
static SESSION_BINDING_DELAY_MS: AtomicU64 = AtomicU64::new(0);

/// While it is held, every Listener of every broker in the process waits `delay` before it binds its connection to the
/// session it was registered to: the window between a registration and that binding is made as long as a test needs.
/// Dropped, the Listeners bind at once again. The broker tests run one broker at a time (`tests/support`, `serial`).
#[must_use = "the delay lasts only while it is held"]
pub struct SessionBindingDelay(());

impl SessionBindingDelay {
    pub fn hold(delay: Duration) -> Self {
        SESSION_BINDING_DELAY_MS.store(delay.as_millis() as u64, Ordering::SeqCst);
        Self(())
    }
}

impl Drop for SessionBindingDelay {
    fn drop(&mut self) {
        SESSION_BINDING_DELAY_MS.store(0, Ordering::SeqCst);
    }
}

/// Waits for as long as a test holds a [`SessionBindingDelay`] for.
pub(crate) async fn before_session_binding() {
    let delay = SESSION_BINDING_DELAY_MS.load(Ordering::SeqCst);
    if delay > 0 {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
}
