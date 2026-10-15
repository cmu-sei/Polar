#![deny(clippy::disallowed_methods)]

pub mod admission;
pub mod agent;
pub mod control;
pub mod desired;
pub mod proposals;
pub mod settings;
pub mod supervisor;

pub const SERVICE_NAME: &str = "polar.git.controller";
