#![allow(clippy::incompatible_msrv)]
use cassini_broker::agent::CassiniBroker;
use std::process::ExitCode;

// The one `main` every agent has: it answers `--help`, `--scaffold` and `--check`, judges the
// configuration, and starts the broker. A broker that ends has failed, so it exits 1 whether it
// returned an error or not; the Job starts a fresh pod, which gets fresh certificates.
fn main() -> ExitCode {
    polar_agent_builder::run::<CassiniBroker>()
}
