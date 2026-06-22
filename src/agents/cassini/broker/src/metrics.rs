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
