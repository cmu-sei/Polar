use kube_observer::agent::KubeObserver;
use std::process::ExitCode;

/// The agent says what it is in `KubeObserver`; `run` answers the command line (`--help`,
/// `--scaffold`, `--check`), judges the configuration and starts it. It never exits 0 for a
/// started agent.
fn main() -> ExitCode {
    polar_agent_builder::run::<KubeObserver>()
}
