#![deny(clippy::disallowed_methods)]

pub mod agent;
pub mod ops;
pub mod supervisor;

pub const SERVICE_NAME: &str = "git.repositories.processor";
