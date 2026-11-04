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

//! How many builds run at once.
//!
//! Every build is a Job with a pipeline that may use several CPUs and gigabytes, so the number running together is
//! capped: a request over the cap waits its turn rather than exhausting the node, and is started when a build ends.
//! The cap is a hardening setting in its own right (`max_concurrent_builds`). Today it is also held down by the
//! jobs' identities: a job's certificate is that of one of a fixed set of ServiceAccounts, and the broker allows one
//! session per identity, so no more builds may run than there are accounts (`backend.kubernetes.accounts`).

use std::collections::{HashSet, VecDeque};

use orchestrator_core::types::BuildRequest;
use uuid::Uuid;

/// What became of a request.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // a request is moved once, into the build it starts or the queue it waits in
pub enum Admitted {
    /// There is room: start it now.
    Now(BuildRequest),
    /// There is not; it is held, at this place in the line (1 is next).
    Waiting(usize),
    /// A build of this id is already running or waiting.
    Duplicate,
}

pub struct Admission {
    cap: usize,
    running: HashSet<Uuid>,
    waiting: VecDeque<BuildRequest>,
}

impl Admission {
    /// At most `cap` builds at once, and at least one so that something can run.
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            running: HashSet::new(),
            waiting: VecDeque::new(),
        }
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn running(&self) -> usize {
        self.running.len()
    }

    pub fn request(&mut self, request: BuildRequest) -> Admitted {
        let id = request.build_id;
        if self.running.contains(&id) || self.waiting.iter().any(|w| w.build_id == id) {
            return Admitted::Duplicate;
        }
        if self.running.len() < self.cap {
            self.running.insert(id);
            Admitted::Now(request)
        } else {
            self.waiting.push_back(request);
            Admitted::Waiting(self.waiting.len())
        }
    }

    /// A build ended, however it did. The request now at the front of the line, if there is one and there is room,
    /// is the next to start.
    pub fn finished(&mut self, build_id: Uuid) -> Option<BuildRequest> {
        self.running.remove(&build_id);
        if self.running.len() >= self.cap {
            return None;
        }
        let next = self.waiting.pop_front()?;
        self.running.insert(next.build_id);
        Some(next)
    }

    /// Give up on the builds still waiting, as the orchestrator shuts down. Those running are not touched.
    pub fn abandon_waiting(&mut self) -> Vec<BuildRequest> {
        self.waiting.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::types::SourceRef;

    fn request() -> BuildRequest {
        BuildRequest {
            build_id: Uuid::new_v4(),
            source: SourceRef::Git {
                url: "https://example.test/r.git".into(),
                commit_sha: "a".repeat(40),
            },
            pipeline_image: None,
            requested_by: "test".into(),
            requested_at: chrono::Utc::now(),
            target_registry: String::new(),
            metadata: Default::default(),
            command: vec![],
        }
    }

    #[test]
    fn builds_start_until_the_cap_and_then_wait_in_order() {
        let mut a = Admission::new(2);
        let (r1, r2, r3, r4) = (request(), request(), request(), request());
        assert!(matches!(a.request(r1.clone()), Admitted::Now(_)));
        assert!(matches!(a.request(r2.clone()), Admitted::Now(_)));
        assert!(matches!(a.request(r3.clone()), Admitted::Waiting(1)));
        assert!(matches!(a.request(r4.clone()), Admitted::Waiting(2)));
        assert_eq!(a.running(), 2);

        assert_eq!(
            a.finished(r1.build_id).map(|r| r.build_id),
            Some(r3.build_id)
        );
        assert_eq!(
            a.finished(r2.build_id).map(|r| r.build_id),
            Some(r4.build_id)
        );
        assert!(a.finished(r3.build_id).is_none());
        assert_eq!(a.running(), 1);
    }

    #[test]
    fn a_build_that_ends_while_others_wait_is_replaced_only_once() {
        let mut a = Admission::new(1);
        let (r1, r2, r3) = (request(), request(), request());
        a.request(r1.clone());
        a.request(r2.clone());
        a.request(r3.clone());
        assert_eq!(
            a.finished(r1.build_id).map(|r| r.build_id),
            Some(r2.build_id)
        );
        // The same build ending again changes nothing: r2 holds the one place.
        assert!(a.finished(r1.build_id).is_none());
        assert_eq!(a.running(), 1);
    }

    #[test]
    fn a_request_seen_twice_is_one_build() {
        let mut a = Admission::new(1);
        let (r1, r2) = (request(), request());
        a.request(r1.clone());
        a.request(r2.clone());
        assert!(matches!(a.request(r1), Admitted::Duplicate));
        assert!(matches!(a.request(r2), Admitted::Duplicate));
    }

    #[test]
    fn the_cap_is_at_least_one() {
        let mut a = Admission::new(0);
        assert_eq!(a.cap(), 1);
        assert!(matches!(a.request(request()), Admitted::Now(_)));
    }

    #[test]
    fn shutting_down_gives_up_on_the_line_and_leaves_the_running() {
        let mut a = Admission::new(1);
        a.request(request());
        a.request(request());
        a.request(request());
        assert_eq!(a.abandon_waiting().len(), 2);
        assert_eq!(a.running(), 1);
    }
}
