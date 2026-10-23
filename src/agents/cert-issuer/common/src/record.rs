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

//! The certificate record (docs/architecture/cert-issuer-isolation.md, "Step 2"): every certificate cert-issuer signs,
//! as it writes it to its log and as it answers the credentials agent, which asks for it over Cassini so that no
//! certificate cert-issuer signs stays unknown to it. These are the words both sides share.

use crate::CertType;
use serde::{Deserialize, Serialize};

/// The credentials agent asks on this topic; cert-issuer answers only a request the broker says it published.
pub const RECORD_REQUESTS_TOPIC: &str = "cert-issuer-record-requests";
/// cert-issuer answers on this topic; the credentials agent takes an answer only from cert-issuer.
pub const RECORD_REPLIES_TOPIC: &str = "cert-issuer-record-replies";
/// The most entries one answer carries.
pub const RECORD_PAGE: usize = 100;

/// How a certificate came to be signed.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum IssuePath {
    /// On a token alone, by the floor: a bootstrap service's, cert-issuer's own client certificate among them.
    TokenOnly,
    /// Vouched for by the credentials agent, by its verified mTLS certificate.
    Vouched,
    /// On `/internal/issue`: an account's, asked for by the credentials agent.
    Internal,
    /// cert-issuer's own TLS server certificate, which it signs for itself at start.
    Own,
}

/// One certificate cert-issuer signed, as its log holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordEntry {
    /// The log it is in: made with the log, so a lost or replaced log is told from one that goes on.
    pub log: String,
    /// Its place in the log, from 1, rising.
    pub seq: u64,
    pub serial: String,
    /// The identity it names: its first DNS name.
    pub identity: String,
    pub cert_type: CertType,
    pub path: IssuePath,
    /// The pod instance it was signed for, when a token said so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_binding: Option<String>,
    /// Unix seconds.
    pub signed_at: i64,
    pub not_after: i64,
    /// The certificate itself. Public: it is presented in every handshake.
    pub certificate_pem: String,
}

/// The credentials agent's question: every entry after `after` in `log`. A log that is not cert-issuer's present one
/// (or none, at first) is answered from its first entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordRequest {
    #[serde(default)]
    pub log: Option<String>,
    pub after: u64,
}

/// cert-issuer's answer: at most [`RECORD_PAGE`] entries after the request's, in order, from its present log, and
/// whether there are more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordReply {
    pub log: String,
    pub entries: Vec<RecordEntry>,
    pub more: bool,
}
