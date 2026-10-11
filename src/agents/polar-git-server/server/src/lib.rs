#![deny(clippy::disallowed_methods)]
//! The Polar git server: git smart HTTP over mTLS for Polar's configuration repositories.
//! Design: `docs/architecture/git-server.md`.

pub mod access;
pub mod admin;
pub mod agent;
pub mod config;
pub mod git;
pub mod http;
pub mod notify;
pub mod pktline;
pub mod settings;
pub mod store;
pub mod supervisor;
pub mod tls;
