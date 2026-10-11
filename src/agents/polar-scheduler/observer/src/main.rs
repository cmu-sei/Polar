use polar_scheduler_observer::agent::SchedulerObserver;
use std::process::ExitCode;

fn main() -> ExitCode {
    polar_agent_builder::run::<SchedulerObserver>()
}
