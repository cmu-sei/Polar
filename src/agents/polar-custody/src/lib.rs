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

//! The custody daemon (docs/architecture/key-custody.md): one in every pod, holding the pod's mTLS keys and doing every
//! signature with them, so the workload holds a certificate and a socket and never the key.
//!
//! - [`tls`]: the one kind of message it signs, a TLS 1.3 CertificateVerify input.
//! - [`key`]: a slot's key, made here and never given out.
//! - [`custody`]: the slots and what each caller may ask of them, without the socket.
//! - [`protocol`]: the requests and replies, and how they are framed.
//! - [`server`]: the socket, the caller check, and requests in flight at once.
//! - [`client`]: the blocking client a workload uses, and a rustls signing key over it.
//! - [`own`]: the daemon's own handshake, its watcher's, signed in process.

pub mod client;
pub mod custody;
pub mod environment;
pub mod key;
pub mod own;
pub mod protocol;
pub mod server;
pub mod tls;
