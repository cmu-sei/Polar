use polar_scheduler_common::{AdhocAgentAnnouncement, ConfigurationOutcome, GitScheduleChange};

// Internal messages for the processor
#[derive(Debug, Clone)]
pub enum ProcessorMsg {
    GitChange(GitScheduleChange),
    Announcement(AdhocAgentAnnouncement),
    Event {
        topic: String,
        payload: Vec<u8>,
    },
    /// An agent says what it did with a signed configuration document. `reported_by` is who the broker
    /// says published it, which is not necessarily who the outcome says it is about.
    Outcome {
        outcome: ConfigurationOutcome,
        reported_by: Option<String>,
    },
}
