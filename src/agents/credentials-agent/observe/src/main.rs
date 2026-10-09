use credentials_agent_observer::agent::CredentialsObserver;
use std::process::ExitCode;

/// The agent says what it is in `CredentialsObserver`; `run` answers the command line (`--help`,
/// `--scaffold`, `--check`), judges the configuration and starts it.
fn main() -> ExitCode {
    polar_agent_builder::run::<CredentialsObserver>()
}
