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

//! Checking the credentials agent's authority certificate, the one that makes a snapshot's key its own
//! (docs/architecture/authority-distribution.md, "Pinning the key to the CA"), against real certificates.

use polar_signing::testing::{
    ESCROW_PUBLIC_KEY_PEM, EscrowSpec, authority_spec, escrow_certificate, now, root,
};
use polar_signing::{Trust, VerifyError, verify_authority_certificate};
use proptest::prelude::*;
use rcgen::{ExtendedKeyUsagePurpose, KeyUsagePurpose};

const CREDENTIALS: &str = "credentials-agent.polar.serviceaccount.cluster.local";

fn trust(pem: &str) -> Trust {
    Trust::from_pem(&[pem.to_string()]).unwrap()
}

fn same_key(a: &str, b: &str) -> bool {
    let squash = |s: &str| s.split_whitespace().collect::<String>();
    squash(a) == squash(b)
}

fn document_signing() -> ExtendedKeyUsagePurpose {
    ExtendedKeyUsagePurpose::Other(cert_issuer_common::OID_DOCUMENT_SIGNING.to_vec())
}

#[test]
fn the_credentials_agent_s_signing_certificate_gives_its_key() {
    let ca = root("Polar Internal CA");
    let cert = escrow_certificate(&ca, authority_spec(CREDENTIALS));
    let key = verify_authority_certificate(&cert, &trust(&ca.pem), now(), CREDENTIALS).unwrap();
    assert!(same_key(&key, ESCROW_PUBLIC_KEY_PEM), "{key}");
}

#[test]
fn a_certificate_from_a_ca_the_leaf_does_not_trust_is_refused() {
    let ca = root("Polar Internal CA");
    let attacker = root("Polar Internal CA");
    let cert = escrow_certificate(&attacker, authority_spec(CREDENTIALS));
    assert_eq!(
        verify_authority_certificate(&cert, &trust(&ca.pem), now(), CREDENTIALS).unwrap_err(),
        VerifyError::UntrustedRoot
    );
}

#[test]
fn another_identity_s_signing_certificate_is_refused() {
    let ca = root("Polar Internal CA");
    let cert = escrow_certificate(
        &ca,
        authority_spec("git-repo-observer.polar.serviceaccount.cluster.local"),
    );
    assert!(matches!(
        verify_authority_certificate(&cert, &trust(&ca.pem), now(), CREDENTIALS),
        Err(VerifyError::WrongIdentity { .. })
    ));
}

#[test]
fn an_escrow_or_a_tls_certificate_of_the_credentials_agent_is_not_its_authority_certificate() {
    let ca = root("Polar Internal CA");
    for (what, spec) in [
        (
            "an escrow certificate's usage, with digital signature",
            EscrowSpec {
                extended_key_usages: vec![ExtendedKeyUsagePurpose::EmailProtection],
                ..authority_spec(CREDENTIALS)
            },
        ),
        (
            "a client certificate",
            EscrowSpec {
                extended_key_usages: vec![ExtendedKeyUsagePurpose::ClientAuth],
                ..authority_spec(CREDENTIALS)
            },
        ),
        (
            "signing and TLS at once",
            EscrowSpec {
                extended_key_usages: vec![document_signing(), ExtendedKeyUsagePurpose::ServerAuth],
                ..authority_spec(CREDENTIALS)
            },
        ),
        (
            "no extended key usage, so any",
            EscrowSpec {
                extended_key_usages: vec![],
                ..authority_spec(CREDENTIALS)
            },
        ),
    ] {
        let cert = escrow_certificate(&ca, spec);
        assert!(
            matches!(
                verify_authority_certificate(&cert, &trust(&ca.pem), now(), CREDENTIALS),
                Err(VerifyError::NotASigningCertificate(_))
            ),
            "{what}"
        );
    }
}

#[test]
fn the_credentials_agent_s_escrow_certificate_is_not_its_authority_certificate() {
    let ca = root("Polar Internal CA");
    let cert = escrow_certificate(
        &ca,
        EscrowSpec {
            identity: CREDENTIALS,
            ..EscrowSpec::default()
        },
    );
    assert!(verify_authority_certificate(&cert, &trust(&ca.pem), now(), CREDENTIALS).is_err());
}

#[test]
fn a_certificate_without_digital_signature_or_out_of_date_or_a_ca_is_refused() {
    let ca = root("Polar Internal CA");
    let cert = escrow_certificate(
        &ca,
        EscrowSpec {
            key_usages: vec![KeyUsagePurpose::KeyEncipherment],
            ..authority_spec(CREDENTIALS)
        },
    );
    assert_eq!(
        verify_authority_certificate(&cert, &trust(&ca.pem), now(), CREDENTIALS).unwrap_err(),
        VerifyError::KeyUsage
    );
    let cert = escrow_certificate(
        &ca,
        EscrowSpec {
            not_before_year: 2020,
            not_after_year: 2025,
            ..authority_spec(CREDENTIALS)
        },
    );
    assert!(matches!(
        verify_authority_certificate(&cert, &trust(&ca.pem), now(), CREDENTIALS),
        Err(VerifyError::NotValidNow(_))
    ));
    let cert = escrow_certificate(
        &ca,
        EscrowSpec {
            is_ca: true,
            ..authority_spec(CREDENTIALS)
        },
    );
    assert_eq!(
        verify_authority_certificate(&cert, &trust(&ca.pem), now(), CREDENTIALS).unwrap_err(),
        VerifyError::LeafIsCa
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// A certificate makes its key the credentials agent's exactly when it chains to the trusted CA, is valid now, names
    /// the credentials agent, is for document signing and no TLS role, permits digital signatures and is not a CA.
    #[test]
    fn a_key_is_the_credentials_agent_s_exactly_when_every_condition_holds(
        trusted in any::<bool>(),
        current in any::<bool>(),
        named in any::<bool>(),
        signing in any::<bool>(),
        digital in any::<bool>(),
        leaf in any::<bool>(),
    ) {
        let ca = root("Polar Internal CA");
        let other = root("Polar Internal CA");
        let spec = EscrowSpec {
            identity: if named { CREDENTIALS } else { "git-repo-observer.polar.serviceaccount.cluster.local" },
            key_usages: if digital { vec![KeyUsagePurpose::DigitalSignature] } else { vec![KeyUsagePurpose::KeyEncipherment] },
            extended_key_usages: if signing { vec![document_signing()] } else { vec![ExtendedKeyUsagePurpose::EmailProtection] },
            not_before_year: if current { 2024 } else { 2020 },
            not_after_year: if current { 2100 } else { 2025 },
            is_ca: !leaf,
        };
        let cert = escrow_certificate(if trusted { &ca } else { &other }, spec);
        let believed = verify_authority_certificate(&cert, &trust(&ca.pem), now(), CREDENTIALS).is_ok();
        prop_assert_eq!(believed, trusted && current && named && signing && digital && leaf);
    }
}
