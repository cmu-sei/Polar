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

//! Properties of signing under a domain: what is signed for one purpose verifies for that purpose only.

use polar_signing::document::Domain;
use polar_signing::testing::{self as support, Kind, Spec, now, root, signer};
use polar_signing::{Trust, VerifyError, sign_in, verify_in};
use proptest::prelude::*;
use serde_json::{Map, Value};
use std::sync::OnceLock;

struct Fixture {
    ca: support::Authority,
    signers: Vec<support::Signer>,
}

/// One CA and a signer of each key type, made once: key generation is what is slow, not what is under test.
fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let ca = root("Polar Internal CA");
        let signers = [Kind::P256, Kind::Ed25519]
            .into_iter()
            .map(|kind| {
                signer(
                    &ca,
                    Spec {
                        kind,
                        ..Spec::default()
                    },
                )
            })
            .collect();
        Fixture { ca, signers }
    })
}

/// A domain line: a short purpose name and its newline.
fn domain() -> impl Strategy<Value = Domain> {
    "[a-z][a-z0-9-]{0,24}".prop_map(|name| {
        let line: &'static [u8] = Box::leak(format!("{name}\n").into_bytes().into_boxed_slice());
        Domain::new(line)
    })
}

/// A payload as configuration and offers have them: an object of integers, strings, and one level of objects.
fn payload() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        any::<i64>().prop_map(Value::from),
        "[ -~]{0,12}".prop_map(Value::from),
    ];
    let field = prop_oneof![
        leaf.clone(),
        prop::collection::btree_map("[a-z_]{1,8}", leaf, 0..4)
            .prop_map(|m| Value::Object(m.into_iter().collect::<Map<_, _>>())),
    ];
    prop::collection::btree_map("[a-z_]{1,8}", field, 0..5)
        .prop_map(|m| Value::Object(m.into_iter().collect::<Map<_, _>>()))
}

proptest! {
    #[test]
    fn a_signature_verifies_under_its_own_domain_and_no_other(
        payload in payload(),
        signed_under in domain(),
        checked_under in domain(),
        which in 0usize..2,
    ) {
        let fixture = fixture();
        let s = &fixture.signers[which];
        let trust = Trust::from_pem(std::slice::from_ref(&fixture.ca.pem)).unwrap();
        let document =
            sign_in(signed_under, payload, std::slice::from_ref(&s.cert_pem), &s.key_pem).unwrap();
        prop_assert!(verify_in(signed_under, &document, &trust, now()).is_ok());
        let checked = verify_in(checked_under, &document, &trust, now());
        if checked_under == signed_under {
            prop_assert!(checked.is_ok());
        } else {
            prop_assert_eq!(checked, Err(VerifyError::BadSignature));
        }
    }
}
