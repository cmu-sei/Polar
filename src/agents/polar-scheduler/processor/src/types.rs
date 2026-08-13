use polar_scheduler_common::{AdhocAgentAnnouncement, GitScheduleChange};

// Internal messages for the processor
#[derive(Debug, Clone)]
pub enum ProcessorMsg {
    GitChange(GitScheduleChange),
    Announcement(AdhocAgentAnnouncement),
    Event { topic: String, payload: Vec<u8> },
}
