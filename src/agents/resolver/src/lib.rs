#![deny(clippy::disallowed_methods)]

//! The OCI resolver: follows image references other agents discover to their registry, pulls the
//! manifest, and publishes what it found for the build processor's provenance linker. See the
//! README.

pub mod agent;
pub mod auth;
pub mod resolver;
pub mod settings;
pub mod supervisor;

/// The name of the resolver's Cassini client.
pub const BROKER_CLIENT_NAME: &str = "polar.oci.resolver.tcp";
pub const RESOLVER_SUPERVISOR_NAME: &str = "polar.oci.resolver.supervisor";
