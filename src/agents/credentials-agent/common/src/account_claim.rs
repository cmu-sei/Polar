//! The account claim, in two phases, for every way of proving who you are.
//!
//! The certificate's identity is the account_id, which the consumer minted
//! when the account was provisioned. A CSR is signed by the claimant's own
//! key, so its SAN has to carry that UUID before it is signed, and the
//! claimant can't know it yet. Hence two phases:
//!
//! 1. Begin: the claimant proves who it is; the consumer replies with the
//!    account_id of the pending provisioning. Repeating this is safe and
//!    returns the same answer until a certificate has been issued.
//! 2. Complete: the claimant sends a CSR whose SAN is that account_id and
//!    proves who it is again. The consumer signs through /internal/issue
//!    and records AccountCertificateIssued, which consumes the pending
//!    record -- so a claim can succeed exactly once.
//!
//! The account_id is not a secret (it ends up in a public certificate), so
//! handing it to a claimant who has already proved who they are leaks
//! nothing.

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// How the claimant proves who they are.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub enum Claimant {
    /// A native user: username and email must match what the admin
    /// provisioned. There is no out-of-band secret.
    Native { username: String, email: String },
    /// The one-time bootstrap admin: a Kubernetes service-account token.
    Bootstrap { credential: String },
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub enum AccountRequest {
    Begin { claimant: Claimant },
    Complete { claimant: Claimant, csr_pem: String },
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub enum AccountReply {
    /// Reply to Begin: build the CSR with this as its SAN.
    Offered { account_id: String },
    /// Reply to Complete.
    Issued {
        certificate_pem: String,
        ca_chain_pem: String,
    },
    Refused { reason: AccountRefusal },
}

/// Deliberately coarse: a native claim that matches nothing doesn't say
/// whether the username or the email was wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize)]
pub enum AccountRefusal {
    /// No pending provisioning matches the claimant.
    NoMatchingProvisioning,
    /// The Kubernetes token didn't validate.
    TokenInvalid,
    /// The token is valid but isn't the bootstrap service account.
    NotTheBootstrapIdentity,
    /// A bootstrap certificate has already been issued.
    BootstrapAlreadyDone,
    /// The CSR didn't parse, or its SAN isn't the offered account_id.
    CsrInvalid,
    SigningFailed,
    /// The ledger couldn't be consulted or written.
    Unavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_survives_rkyv_round_trip() {
        let request = AccountRequest::Complete {
            claimant: Claimant::Native {
                username: "alice".to_string(),
                email: "alice@example.com".to_string(),
            },
            csr_pem: "csr".to_string(),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request).unwrap();
        let back = rkyv::from_bytes::<AccountRequest, rkyv::rancor::Error>(&bytes).unwrap();
        match back {
            AccountRequest::Complete { claimant, csr_pem } => {
                assert_eq!(csr_pem, "csr");
                assert_eq!(
                    claimant,
                    Claimant::Native {
                        username: "alice".to_string(),
                        email: "alice@example.com".to_string(),
                    }
                );
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }
}
