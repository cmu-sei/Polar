use git_repo_controller::agent::GitController;
use std::process::ExitCode;

// The one `main` every agent has: it answers `--help`, `--scaffold` and `--check`, judges the
// configuration, and starts the controller. An agent that ends has failed, so it exits 1.
fn main() -> ExitCode {
    polar_agent_builder::run::<GitController>()
}
