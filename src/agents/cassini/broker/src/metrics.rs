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

// broker/src/metrics.rs
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct MessageLatency {
    pub trace_id: [u8; 16],
    pub stages: Vec<MessageStage>,
}

#[derive(Debug, Clone)]
pub struct MessageStage {
    pub name: &'static str,
    pub start: Instant,
    pub end: Instant,
    pub actor_name: String,
}

impl MessageLatency {
    pub fn new(trace_id: [u8; 16]) -> Self {
        Self {
            trace_id,
            stages: Vec::new(),
        }
    }
    
    pub fn record_stage(&mut self, name: &'static str, actor_name: String) -> StageGuard {
        let start = Instant::now();
        StageGuard {
            latency: self,
            name,
            actor_name,
            start,
        }
    }
}

pub struct StageGuard<'a> {
    latency: &'a mut MessageLatency,
    name: &'static str,
    actor_name: String,
    start: Instant,
}

impl<'a> Drop for StageGuard<'a> {
    fn drop(&mut self) {
        let stage = MessageStage {
            name: self.name,
            start: self.start,
            end: Instant::now(),
            actor_name: self.actor_name.clone(),
        };
        self.latency.stages.push(stage);
    }
}
