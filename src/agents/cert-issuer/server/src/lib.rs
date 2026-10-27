/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

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

pub mod agent;
pub mod ca;
pub mod config;
pub mod csr;
pub mod floor;
pub mod floor_state;
pub mod handler;
pub mod listener;
pub mod oidc;
pub mod own_certificate;
pub mod record;
pub mod server;
pub mod settings;
pub mod supervisor;
pub mod telemetry;
pub mod tls;

// Re-export wire types so tests in this crate can use them
// without depending on the common crate path.
pub use cert_issuer_common::{
    InternalIssueRequest, IssueError, IssueOutcome, IssueRequest, IssueResponse,
};
