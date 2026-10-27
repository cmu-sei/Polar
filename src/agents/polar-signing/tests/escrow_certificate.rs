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

//! Checking an agent's escrow certificate before encrypting to it, against real certificates.

use polar_signing::testing::{
    ESCROW_PUBLIC_KEY_PEM, EscrowSpec, escrow_certificate, intermediate, now, root,
};
use polar_signing::{Trust, VerifyError, verify_escrow_certificate};
use rcgen::{ExtendedKeyUsagePurpose, KeyUsagePurpose};

const OBSERVER: &str = "git-repo-observer.polar.serviceaccount.cluster.local";

fn trust(pem: &str) -> Trust {
    Trust::from_pem(&[pem.to_string()]).unwrap()
}

fn same_key(a: &str, b: &str) -> bool {
    let squash = |s: &str| s.split_whitespace().collect::<String>();
    squash(a) == squash(b)
}

#[test]
fn the_certificate_of_the_agent_asked_for_gives_its_public_key() {
    let ca = root("Polar Internal CA");
    let cert = escrow_certificate(&ca, EscrowSpec::default());
    let key = verify_escrow_certificate(&cert, &trust(&ca.pem), now(), OBSERVER).unwrap();
    assert!(same_key(&key, ESCROW_PUBLIC_KEY_PEM), "{key}");
}

#[test]
fn another_agents_certificate_is_refused_even_though_it_is_genuine() {
    let ca = root("Polar Internal CA");
    let cert = escrow_certificate(
        &ca,
        EscrowSpec {
            identity: "git-repo-controller.polar.serviceaccount.cluster.local",
            ..EscrowSpec::default()
        },
    );
    let refused = verify_escrow_certificate(&cert, &trust(&ca.pem), now(), OBSERVER).unwrap_err();
    assert!(
        matches!(refused, VerifyError::WrongIdentity { .. }),
        "{refused:?}"
    );
    assert!(refused.to_string().contains(OBSERVER));
}

#[test]
fn a_lookalike_identity_is_not_the_identity() {
    let ca = root("Polar Internal CA");
    for identity in [
        "git-repo-observer.polar.serviceaccount.cluster.local.evil.example",
        "xgit-repo-observer.polar.serviceaccount.cluster.local",
        "GIT-REPO-OBSERVER.polar.serviceaccount.cluster.local",
        "git-repo-observer",
    ] {
        let cert = escrow_certificate(
            &ca,
            EscrowSpec {
                identity,
                ..EscrowSpec::default()
            },
        );
        assert!(
            matches!(
                verify_escrow_certificate(&cert, &trust(&ca.pem), now(), OBSERVER),
                Err(VerifyError::WrongIdentity { .. })
            ),
            "{identity}"
        );
    }
}

#[test]
fn a_certificate_from_a_ca_the_author_does_not_trust_is_refused() {
    // The substitution attack: whoever answers in place of the gateway signs with a CA of their own.
    let ca = root("Polar Internal CA");
    let attacker = root("Polar Internal CA");
    let cert = escrow_certificate(&attacker, EscrowSpec::default());
    assert_eq!(
        verify_escrow_certificate(&cert, &trust(&ca.pem), now(), OBSERVER).unwrap_err(),
        VerifyError::UntrustedRoot
    );
}

#[test]
fn a_certificate_that_is_not_valid_now_is_refused() {
    let ca = root("Polar Internal CA");
    for (from, to) in [(2020, 2025), (2028, 2030)] {
        let cert = escrow_certificate(
            &ca,
            EscrowSpec {
                not_before_year: from,
                not_after_year: to,
                ..EscrowSpec::default()
            },
        );
        assert!(
            matches!(
                verify_escrow_certificate(&cert, &trust(&ca.pem), now(), OBSERVER),
                Err(VerifyError::NotValidNow(_))
            ),
            "{from}-{to}"
        );
    }
}

#[test]
fn a_certificate_not_made_for_encryption_is_refused() {
    let ca = root("Polar Internal CA");
    for usages in [
        vec![KeyUsagePurpose::DigitalSignature],
        vec![],
        vec![KeyUsagePurpose::KeyCertSign],
    ] {
        let cert = escrow_certificate(
            &ca,
            EscrowSpec {
                key_usages: usages,
                ..EscrowSpec::default()
            },
        );
        assert_eq!(
            verify_escrow_certificate(&cert, &trust(&ca.pem), now(), OBSERVER).unwrap_err(),
            VerifyError::NotForEncryption
        );
    }
}

#[test]
fn a_certificate_that_is_a_ca_is_refused() {
    let ca = root("Polar Internal CA");
    let cert = escrow_certificate(
        &ca,
        EscrowSpec {
            is_ca: true,
            key_usages: vec![
                KeyUsagePurpose::KeyEncipherment,
                KeyUsagePurpose::KeyCertSign,
            ],
            ..EscrowSpec::default()
        },
    );
    assert_eq!(
        verify_escrow_certificate(&cert, &trust(&ca.pem), now(), OBSERVER).unwrap_err(),
        VerifyError::LeafIsCa
    );
}

#[test]
fn a_chain_through_an_intermediate_is_followed_and_the_anchor_may_be_included() {
    let ca = root("Polar Internal CA");
    let mid = intermediate(&ca, "Polar Issuing CA", 2040);
    let leaf = escrow_certificate(&mid, EscrowSpec::default());
    let anchors = trust(&ca.pem);

    let chain = format!("{leaf}{}", mid.pem);
    assert!(verify_escrow_certificate(&chain, &anchors, now(), OBSERVER).is_ok());
    let with_anchor = format!("{leaf}{}{}", mid.pem, ca.pem);
    assert!(verify_escrow_certificate(&with_anchor, &anchors, now(), OBSERVER).is_ok());

    // the intermediate left out: nothing links the leaf to the anchor
    assert_eq!(
        verify_escrow_certificate(&leaf, &anchors, now(), OBSERVER).unwrap_err(),
        VerifyError::UntrustedRoot
    );
    // the order matters: the certificate to encrypt to comes first
    let reversed = format!("{}{leaf}", mid.pem);
    assert!(verify_escrow_certificate(&reversed, &anchors, now(), OBSERVER).is_err());
}

#[test]
fn text_that_is_not_a_certificate_is_refused() {
    let ca = root("Polar Internal CA");
    for text in [
        "",
        "not a certificate",
        "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
    ] {
        assert!(
            matches!(
                verify_escrow_certificate(text, &trust(&ca.pem), now(), OBSERVER),
                Err(VerifyError::BadCertificate(_))
            ),
            "{text:?}"
        );
    }
}

#[test]
fn the_key_returned_is_the_agents_and_not_the_cas() {
    let ca = root("Polar Internal CA");
    let cert = escrow_certificate(&ca, EscrowSpec::default());
    let key = verify_escrow_certificate(&cert, &trust(&ca.pem), now(), OBSERVER).unwrap();
    assert!(key.contains("BEGIN PUBLIC KEY"));
    assert!(!same_key(&key, &ca.pem));
}

#[test]
fn a_chain_whose_links_do_not_join_is_refused_even_if_the_last_certificate_is_trusted() {
    // The leaf was issued by one intermediate, but the chain offered behind it is a different, trusted one.
    let ca = root("Polar Internal CA");
    let real = intermediate(&ca, "Polar Issuing CA", 2040);
    let other = intermediate(&ca, "Some Other Issuing CA", 2040);
    let leaf = escrow_certificate(&real, EscrowSpec::default());
    let chain = format!("{leaf}{}", other.pem);
    let refused = verify_escrow_certificate(&chain, &trust(&ca.pem), now(), OBSERVER).unwrap_err();
    assert!(
        matches!(refused, VerifyError::ChainBroken(_)),
        "{refused:?}"
    );
}

#[test]
fn the_anchor_at_the_end_does_not_count_against_the_length_limit() {
    let ca = root("Polar Internal CA");
    let a = intermediate(&ca, "A", 2040);
    let b = intermediate(&a, "B", 2040);
    let c = intermediate(&b, "C", 2040);
    let leaf = escrow_certificate(&c, EscrowSpec::default());
    let anchors = trust(&ca.pem);
    // leaf and three intermediates is the longest chain; the anchor after it is not counted
    let chain = format!("{leaf}{}{}{}", c.pem, b.pem, a.pem);
    assert!(verify_escrow_certificate(&chain, &anchors, now(), OBSERVER).is_ok());
    let with_anchor = format!("{chain}{}", ca.pem);
    assert!(verify_escrow_certificate(&with_anchor, &anchors, now(), OBSERVER).is_ok());
    // one more is too many
    let d = intermediate(&c, "D", 2040);
    let longer = escrow_certificate(&d, EscrowSpec::default());
    let too_long = format!("{longer}{}{}{}{}", d.pem, c.pem, b.pem, a.pem);
    assert!(matches!(
        verify_escrow_certificate(&too_long, &anchors, now(), OBSERVER),
        Err(VerifyError::ChainTooLong(_))
    ));
}

#[test]
fn a_certificate_that_is_not_an_escrow_certificate_by_its_purpose_is_refused() {
    // The escrow certificate is the application layer's; a certificate the transport layer would take, or one with
    // no stated purpose (taken for every use), is not one, whatever its key usage says.
    let ca = root("Polar Internal CA");
    for ekus in [
        vec![],
        vec![ExtendedKeyUsagePurpose::ClientAuth],
        vec![ExtendedKeyUsagePurpose::ServerAuth],
        vec![
            ExtendedKeyUsagePurpose::EmailProtection,
            ExtendedKeyUsagePurpose::ClientAuth,
        ],
        vec![
            ExtendedKeyUsagePurpose::EmailProtection,
            ExtendedKeyUsagePurpose::ServerAuth,
        ],
        vec![
            ExtendedKeyUsagePurpose::EmailProtection,
            ExtendedKeyUsagePurpose::Any,
        ],
        vec![ExtendedKeyUsagePurpose::CodeSigning],
    ] {
        let cert = escrow_certificate(
            &ca,
            EscrowSpec {
                extended_key_usages: ekus.clone(),
                ..EscrowSpec::default()
            },
        );
        assert!(
            matches!(
                verify_escrow_certificate(&cert, &trust(&ca.pem), now(), OBSERVER),
                Err(VerifyError::NotAnEscrowCertificate(_))
            ),
            "{ekus:?}"
        );
    }
}
