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

//! A real, generated keypair -- no longer a mock. Generated once at
//! consumer startup and held for the lifetime of the process.
//!
//! Still genuinely incomplete relative to credentials-agent-architecture.md's
//! full secret-escrow design: no persistence across restarts (a fresh
//! keypair generates on every restart today, which is real, tracked
//! follow-up work -- anything encrypted to a prior process's key
//! becomes unreadable after a restart until real persistence lands),
//! no rotation, no identity-distribution topic pair, and the private
//! key is currently unused (nothing yet performs the decrypt/
//! re-encrypt relay this key is ultimately for). Held anyway, rather
//! than discarded, specifically so that work can reuse this same
//! keypair instead of a second, wasted generation.
//!
//! Deliberately a separate keypair from this agent's own mTLS client
//! certificate -- reusing one asymmetric keypair across two different
//! cryptographic protocols is a real cryptographic-engineering smell
//! even when nothing obviously breaks from doing so.

use rsa::RsaPrivateKey;
use rsa::pkcs8::LineEnding;

pub struct EscrowKeypair {
    #[allow(dead_code)] // unused until the real decrypt/re-encrypt relay is built
    private_key: RsaPrivateKey,
    public_key_pem: String,
}

impl EscrowKeypair {
    pub fn generate() -> Result<Self, String> {
        let mut rng = rsa::rand_core::OsRng;
        let private_key = RsaPrivateKey::new(&mut rng, 2048)
            .map_err(|e| format!("failed to generate escrow keypair: {e}"))?;
        let public_key = private_key.to_public_key();
        let public_key_pem = rsa::pkcs8::EncodePublicKey::to_public_key_pem(
            &public_key,
            LineEnding::LF,
        )
        .map_err(|e| format!("failed to encode escrow public key: {e}"))?;

        Ok(Self {
            private_key,
            public_key_pem,
        })
    }

    pub fn public_key_pem(&self) -> &str {
        &self.public_key_pem
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn generates_a_real_pem_encoded_public_key() {
        let keypair = EscrowKeypair::generate().unwrap();
        assert!(keypair.public_key_pem().starts_with("-----BEGIN PUBLIC KEY-----"));
        assert!(keypair.public_key_pem().ends_with("-----END PUBLIC KEY-----\n"));
    }

    #[test]
    fn two_generated_keypairs_are_genuinely_different() {
        let a = EscrowKeypair::generate().unwrap();
        let b = EscrowKeypair::generate().unwrap();
        assert_ne!(a.public_key_pem(), b.public_key_pem());
    }

    /// Cached across every test that needs *a* valid keypair but
    /// doesn't care about its specific value -- RSA generation is
    /// expensive (>100ms), matching cert-issuer's own established
    /// caching convention for exactly this reason.
    pub(crate) fn test_keypair() -> std::sync::Arc<EscrowKeypair> {
        use std::sync::{Arc, OnceLock};
        static KEYPAIR: OnceLock<Arc<EscrowKeypair>> = OnceLock::new();
        KEYPAIR
            .get_or_init(|| Arc::new(EscrowKeypair::generate().unwrap()))
            .clone()
    }
}
