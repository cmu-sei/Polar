use git_repo_observer::agent::GitObserver;
use std::process::ExitCode;

// The one `main` every agent has: it answers `--help`, `--scaffold` and `--check`, judges the
// configuration, and starts the observer. An agent that ends has failed, so it exits 1.
fn main() -> ExitCode {
    polar_agent_builder::run::<GitObserver>()
}
