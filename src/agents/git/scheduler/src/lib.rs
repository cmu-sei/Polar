#![deny(clippy::disallowed_methods)]

pub mod agent;
pub mod config;
pub mod supervisor;

pub const SERVICE_NAME: &str = "polar.git.scheduler";
