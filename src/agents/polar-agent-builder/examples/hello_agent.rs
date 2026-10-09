//! The smallest agent that implements `PolarAgent`, to read and to ask:
//!
//! ```text
//! cargo run -p polar-agent-builder --example hello_agent -- --help
//! cargo run -p polar-agent-builder --example hello_agent -- --scaffold
//! ```
//!
//! It reads one variable of its own, `HELLO_GREETING`, which shows up in the scaffold beside the
//! builder's, the health actor's and the Cassini client's.

use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use polar_env::{Described, Section};
use std::process::ExitCode;
use std::time::Duration;

static GREETING: polar_env::Defaulted<String> = polar_env::Defaulted::text(
    "HELLO_GREETING",
    "what the agent says when it starts",
    "hello",
);
static VARIABLES: &[&dyn Described] = &[&GREETING];
static HELLO: Section = Section {
    name: "hello",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};
struct Hello;

impl PolarAgent for Hello {
    const NAME: &'static str = "hello-agent";
    const PURPOSE: &'static str = "Says hello and stops; shows how an agent declares itself";
    const NEEDS: &'static [Capability] = &[Capability::Cassini];
    const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);
    const SECTIONS: &'static [&'static Section] = &[&HELLO];
    const HELP: Option<&'static str> =
        Some("It reads HELLO_GREETING from its configuration, prints it, and ends.");

    async fn start(configured: Configured) -> Result<(), StartError> {
        println!("{}", configured.config().read(&GREETING)?);
        Ok(())
    }
}

fn main() -> ExitCode {
    polar_agent_builder::run::<Hello>()
}
