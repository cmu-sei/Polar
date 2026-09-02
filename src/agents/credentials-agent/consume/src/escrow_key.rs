/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

//! A thin wrapper answering "what's your current escrow public key" --
//! core-to-core only, no observer involvement, no failure mode to
//! represent (the escrow keypair always exists once this agent is
//! running, per EscrowKeypair::load_or_generate). Genuinely simpler
//! than issuance or identity-normalization: no cert-issuer call, no
//! ledger interaction, just reading an already-loaded, in-memory key.
//!
//! Auditing this request (event log entry + graph projection) is
//! real, tracked, deliberately deferred work -- not built in this
//! pass.

use crate::escrow::EscrowKeypair;
use credentials_agent_common::escrow_key::{EscrowKeyReply, EscrowKeyRequest};
use std::sync::Arc;

pub struct EscrowKeyHandler {
    pub escrow_keypair: Arc<EscrowKeypair>,
}

impl EscrowKeyHandler {
    pub fn handle(&self, _request: EscrowKeyRequest) -> EscrowKeyReply {
        EscrowKeyReply {
            public_key_pem: self.escrow_keypair.public_key_pem().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_the_actual_loaded_keys_own_public_pem() {
        let keypair = crate::escrow::tests::test_keypair();
        let expected_pem = keypair.public_key_pem().to_string();

        let handler = EscrowKeyHandler {
            escrow_keypair: keypair,
        };

        let reply = handler.handle(EscrowKeyRequest);

        assert_eq!(reply.public_key_pem, expected_pem);
    }
}
