use clap::Parser;
use harness_common::TestPlan;
pub mod actors;
pub mod client;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Arguments {
    #[arg(short, long)]
    /// Path to a valid .dhall configuration file. It will be evaluated to generate a TestPlan
    pub config: String,
}

pub fn read_test_config(path: &str) -> TestPlan {
    // let file = File::open(path).expect("Expected to find a test config at {path}");

    let dhall_str =
        std::fs::read_to_string(path).expect("Expected to find dhall configuration at {path}");

    let config: TestPlan = serde_dhall::from_str(&dhall_str).parse().unwrap();

    return config;
}
