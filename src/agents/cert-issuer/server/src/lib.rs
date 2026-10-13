//! Cert issuer library.
//!
//! Modules are organized so that each can be tested in isolation:
//! - `oidc`: JWT validation against configured issuers, JWKS caching.
//! - `csr`: CSR parsing and SAN-vs-identity verification.
//! - `floor`: who may be issued with only a token (the bootstrap identities).
//! - `ca`: Smallstep CA client (mockable via trait).
//! - `handler`: protocol handler that wires the above together.
//! - `server`: Axum HTTP wrapper around the handler.
//! - `config`: Configuration types loaded at startup.
//! - `telemetry`: Structured event emission.

pub mod ca;
pub mod config;
pub mod csr;
pub mod floor;
pub mod handler;
pub mod oidc;
pub mod server;
pub mod telemetry;
pub mod tls;

// Re-export wire types so tests in this crate can use them
// without depending on the common crate path.
pub use cert_issuer_common::{
    InternalIssueRequest, IssueError, IssueOutcome, IssueRequest, IssueResponse,
};
