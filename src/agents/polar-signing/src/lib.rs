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

#![deny(clippy::disallowed_methods)]
//! Signed configuration documents. Design: `docs/architecture/signed-configuration.md`.
//!
//! A configuration item is a document with exactly two fields: a `payload`, which is everything
//! that has an effect, and a `signature`, which holds only the signature and the signer's
//! certificate chain. The signature covers the canonical encoding of the payload, so nothing that
//! changes behaviour sits outside what was signed.
//!
//! - [`canonical`]: the one byte encoding of a payload that is signed.
//! - [`document`]: the two-field layout and the signature block.
//! - [`sign`]: makes a signed document with a user's certificate and key.
//! - [`verify`]: checks the layout, the chain to a pinned CA, the certificate's validity and use, and
//!   the signature, and says who signed.
//! - [`authority`]: whether that signer may make this change. Separate from verification so a policy
//!   engine can replace the first rule.
//!
//! This crate deals in JSON values. Reading a Dhall file (and refusing one with an import) is the
//! caller's business: a document is signed after it has been rendered to normal form.

pub mod authority;
pub mod canonical;
mod cert;
pub mod document;
pub mod sign;
#[cfg(feature = "testing")]
pub mod testing;
pub mod verify;

pub use authority::{Authority, AuthorityRefusal, RequireCapability};
pub use document::{Algorithm, Document, LayoutError, SignatureBlock};
pub use sign::{SignError, sign};
pub use verify::{SignerInfo, Trust, VerifyError, verify, verify_escrow_certificate};
