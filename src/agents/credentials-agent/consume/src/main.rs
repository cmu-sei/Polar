use credentials_agent_consumer::agent::CredentialsConsumer;
use std::process::ExitCode;

/// The agent says what it is in `CredentialsConsumer`; `run` answers the command line (`--help`,
/// `--scaffold`, `--check`), judges the configuration and starts it.
fn main() -> ExitCode {
    polar_agent_builder::run::<CredentialsConsumer>()
}
