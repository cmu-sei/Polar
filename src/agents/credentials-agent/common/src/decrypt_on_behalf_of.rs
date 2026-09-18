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

//! The real, new case retrieve_secret.rs's own module doc
//! deliberately didn't cover: decrypting something a *different*
//! agent encrypted -- kube-consumer encrypting a real, plaintext
//! Kubernetes env var value to the Credentials Agent's own identity
//! (via CryptoActor's own EncryptForRecipient) and writing the
//! resulting ciphertext directly into the graph, rather than
//! escrowing it via StoreSecret. Some later, real consumer -- not
//! the original writer -- reads that ciphertext straight from the
//! graph and asks to decrypt it here.
//!
//! Genuinely a real authorization decision, not merely a retrieve:
//! unlike RetrieveSecretRequest (where the requester is always
//! authorized, having stored the secret themselves or being the one,
//! real, escrowed owner), a decrypt-on-behalf-of requester is
//! genuinely, potentially a stranger to whatever wrote the
//! ciphertext -- hence the real PolicyActor/PermissionsActor query
//! this request's own, real handler performs before ever touching
//! the escrow private key at all.
//!
//! Mirrors retrieve_secret.rs's own shape deliberately: the same,
//! real re-encrypt-to-the-requester's-own-broker-verified-identity
//! discipline, for the same, real reason (letting a caller specify
//! an arbitrary target key would let a compromised or malicious
//! agent redirect someone else's secret to an attacker-controlled
//! key).

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize)]
pub struct DecryptOnBehalfOfRequest {
    /// The real ciphertext the requester already read directly from
    /// wherever it lives (e.g. a graph node) -- never looked up by
    /// name from this agent's own ledger, unlike RetrieveSecretRequest.
    pub ciphertext: crate::escrow_secret::EscrowedSecret,
    /// The real Cedar resource UID whoever originally encrypted this
    /// value chose at encrypt time (e.g.
    /// `KubeSecret::"namespace/pod/ENV_VAR"`) -- the real, stable
    /// identifier a policy is actually written against, travelling
    /// alongside the ciphertext specifically so policy and data can
    /// never silently drift apart.
    pub resource_id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize)]
pub enum DecryptOnBehalfOfReply {
    /// A real, genuine policy decision permitted this -- the same,
    /// real secret, decrypted and immediately re-encrypted to the
    /// requester's own, broker-verified public key.
    Allowed {
        secret: crate::escrow_secret::EscrowedSecret,
    },
    /// The request was genuinely valid and the requester genuinely
    /// verified -- but a real, live PolicyActor decision refused it.
    /// Deliberately, genuinely distinct from Refused below: this is
    /// a real, deliberate policy outcome, not a structural failure.
    Denied {
        reason: String,
    },
    /// Mirrors RetrieveSecretReply::Refused's own, real reasoning
    /// exactly -- a structural failure that happens before policy is
    /// even genuinely consulted.
    Refused {
        reason: DecryptOnBehalfOfRefusalReason,
    },
    /// Mirrors RetrieveSecretReply::Failed's own, real distinction:
    /// not a deterministic rejection retrying would ever fix.
    Failed {
        detail: String,
    },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub enum DecryptOnBehalfOfRefusalReason {
    /// Mirrors RetrieveRefusalReason::AnonymousPublisherNotAllowed's
    /// own, real reasoning exactly.
    AnonymousPublisherNotAllowed,
    /// Mirrors RetrieveRefusalReason::RequesterCertificateNotFound's
    /// own, real reasoning exactly.
    RequesterCertificateNotFound,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escrow_secret::EscrowedSecret;

    fn sample_secret() -> EscrowedSecret {
        let mut rng = rand::rng();
        let private_key = rsa::RsaPrivateKey::new(&mut rng, 4096).unwrap();
        let public_key_pem = rsa::pkcs8::EncodePublicKey::to_public_key_pem(
            &private_key.to_public_key(),
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap();
        EscrowedSecret::encrypt(b"a-real-secret", &public_key_pem).unwrap()
    }

    #[test]
    fn decrypt_on_behalf_of_request_round_trips_through_serde_json() {
        let original = DecryptOnBehalfOfRequest {
            ciphertext: sample_secret(),
            resource_id: r#"KubeSecret::"namespace/pod/ENV_VAR""#.to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: DecryptOnBehalfOfRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn decrypt_on_behalf_of_request_round_trips_through_rkyv() {
        let original = DecryptOnBehalfOfRequest {
            ciphertext: sample_secret(),
            resource_id: r#"KubeSecret::"namespace/pod/ENV_VAR""#.to_string(),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: DecryptOnBehalfOfRequest =
            rkyv::from_bytes::<DecryptOnBehalfOfRequest, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn decrypt_on_behalf_of_reply_allowed_round_trips_through_both() {
        let original = DecryptOnBehalfOfReply::Allowed {
            secret: sample_secret(),
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: DecryptOnBehalfOfReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: DecryptOnBehalfOfReply =
            rkyv::from_bytes::<DecryptOnBehalfOfReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored_rkyv);
    }

    #[test]
    fn decrypt_on_behalf_of_reply_denied_round_trips_through_both() {
        let original = DecryptOnBehalfOfReply::Denied {
            reason: "no permit rule matched".to_string(),
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: DecryptOnBehalfOfReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: DecryptOnBehalfOfReply =
            rkyv::from_bytes::<DecryptOnBehalfOfReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored_rkyv);
    }

    #[test]
    fn decrypt_on_behalf_of_reply_refused_round_trips_with_correct_reason() {
        let original = DecryptOnBehalfOfReply::Refused {
            reason: DecryptOnBehalfOfRefusalReason::RequesterCertificateNotFound,
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: DecryptOnBehalfOfReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: DecryptOnBehalfOfReply =
            rkyv::from_bytes::<DecryptOnBehalfOfReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored_rkyv);
    }

    #[test]
    fn decrypt_on_behalf_of_reply_failed_round_trips_through_both() {
        let original = DecryptOnBehalfOfReply::Failed {
            detail: "policy actor unreachable".to_string(),
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: DecryptOnBehalfOfReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: DecryptOnBehalfOfReply =
            rkyv::from_bytes::<DecryptOnBehalfOfReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored_rkyv);
    }
}
