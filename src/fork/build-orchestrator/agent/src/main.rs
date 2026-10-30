use build_orchestrator::agent::BuildOrchestrator;
use std::process::ExitCode;

fn main() -> ExitCode {
    polar_agent_builder::run::<BuildOrchestrator>()
}
