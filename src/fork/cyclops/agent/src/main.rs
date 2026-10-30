use build_orchestrator::agent::Cyclops;
use std::process::ExitCode;

fn main() -> ExitCode {
    polar_agent_builder::run::<Cyclops>()
}
