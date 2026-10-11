use polar_scheduler::agent::SchedulerProcessor;
use std::process::ExitCode;

fn main() -> ExitCode {
    polar_agent_builder::run::<SchedulerProcessor>()
}
