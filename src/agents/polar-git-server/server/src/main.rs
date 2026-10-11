use polar_git_server::agent::GitServer;
use std::process::ExitCode;

fn main() -> ExitCode {
    polar_agent_builder::run::<GitServer>()
}
