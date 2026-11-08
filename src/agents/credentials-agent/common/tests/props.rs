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

//! Properties of the signed session offer: whatever the credentials agent offers, a workload believes it as
//! offered, and believes no change to it.

use credentials_agent_common::session_offer::{OfferCheck, OfferRefusal, OfferSigner};
use credentials_agent_common::workload_issuance::SessionOffer;
use polar_signing::VerifyError;
use polar_signing::testing::{Kind, Spec, now, root, signer};
use proptest::prelude::*;
use std::sync::OnceLock;

const CREDENTIALS_AGENT: &str = "credentials-agent.polar.serviceaccount.cluster.local";

/// One CA and a signing certificate of each key type for the credentials agent, made once.
fn fixture() -> &'static (OfferCheck, Vec<OfferSigner>) {
    static FIXTURE: OnceLock<(OfferCheck, Vec<OfferSigner>)> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let ca = root("Polar Internal CA");
        let signers = [Kind::Ed25519, Kind::P256]
            .into_iter()
            .map(|kind| {
                let s = signer(
                    &ca,
                    Spec {
                        kind,
                        identity: CREDENTIALS_AGENT,
                        capabilities: vec![],
                        extended_key_usages: vec![rcgen::ExtendedKeyUsagePurpose::Other(
                            cert_issuer_common::OID_DOCUMENT_SIGNING.to_vec(),
                        )],
                        ..Spec::default()
                    },
                );
                OfferSigner::new(vec![s.cert_pem], s.key_pem).unwrap()
            })
            .collect();
        let check = OfferCheck::new(&ca.pem, CREDENTIALS_AGENT.to_string()).unwrap();
        (check, signers)
    })
}

fn offer() -> impl Strategy<Value = (String, [u8; 32], u64, usize)> {
    ("[ -~]{0,40}", any::<[u8; 32]>(), any::<u64>(), 0usize..2)
}

/// One field of an offer changed to something else.
#[derive(Debug, Clone)]
enum Change {
    SessionId(String),
    PublicKey(usize, u8),
    Lifetime(u64),
}

fn change() -> impl Strategy<Value = Change> {
    prop_oneof![
        "[ -~]{0,40}".prop_map(Change::SessionId),
        (0usize..32, 1u8..=255).prop_map(|(i, x)| Change::PublicKey(i, x)),
        any::<u64>().prop_map(Change::Lifetime),
    ]
}

/// Applies the change; false if it changed nothing (the same session id or lifetime drawn again).
fn apply(offer: &mut SessionOffer, change: &Change) -> bool {
    match change {
        Change::SessionId(id) if *id != offer.session_id => offer.session_id = id.clone(),
        Change::PublicKey(i, x) => offer.public_key[*i] ^= x,
        Change::Lifetime(n) if *n != offer.valid_for_secs => offer.valid_for_secs = *n,
        _ => return false,
    }
    true
}

proptest! {
    #[test]
    fn whatever_is_offered_is_believed_as_offered_and_as_it_travels((id, key, secs, which) in offer()) {
        let (check, signers) = fixture();
        let offer = signers[which].sign(id, key, secs).unwrap();
        prop_assert_eq!(check.check(&offer, now()), Ok(()));
        let travelled: SessionOffer = serde_json::from_str(&serde_json::to_string(&offer).unwrap()).unwrap();
        prop_assert_eq!(&travelled, &offer);
        prop_assert_eq!(check.check(&travelled, now()), Ok(()));
    }

    #[test]
    fn no_change_to_an_offer_is_believed((id, key, secs, which) in offer(), change in change()) {
        let (check, signers) = fixture();
        let mut offer = signers[which].sign(id, key, secs).unwrap();
        prop_assume!(apply(&mut offer, &change));
        prop_assert_eq!(
            check.check(&offer, now()),
            Err(OfferRefusal::Signature(VerifyError::BadSignature))
        );
    }
}
