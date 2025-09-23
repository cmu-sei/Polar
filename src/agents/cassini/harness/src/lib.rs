use clap::Parser;
use ractor::async_trait;

pub mod actors;

// pub const BROKER_CLIENT_NAME: &str = "cassini.harness.client";

/// HarnessSupervisor CLI — generates messages for the broker
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Name of the topic to publish to
    #[arg(long)]
    pub topic: String,

    /// Message size in bytes
    #[arg(long, default_value_t = 256)]
    pub msg_size: usize,

    /// Message type (string, int, enum, etc.)
    #[arg(long, default_value = "string")]
    pub msg_type: String,

    /// Messages per second
    #[arg(long, default_value_t = 100)]
    pub rate: u32,

    /// Duration to run in seconds
    #[arg(long, default_value_t = 30)]
    pub duration: u64,

    /// Whether to conduct a dry run test. When set, messages will not be sent, but outputted to stdout
    pub dry_run: bool,
}
