use harness_common::TestPlan;
use serde_dhall::SimpleValue;

pub mod actors;
pub mod client;

pub fn read_test_config(path: &str) -> TestPlan {
    // let file = File::open(path).expect("Expected to find a test config at {path}");

    let dhall_str =
        std::fs::read_to_string(path).expect("Expected to find dhall configuration at {path}");

    let config: TestPlan = serde_dhall::from_str(&dhall_str).parse().unwrap();

    return config;
}
