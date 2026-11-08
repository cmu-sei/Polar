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

//! Properties of the escrow envelope, for both suites: what is sealed opens to what was sealed, under its key and its
//! context only, and nothing a tamperer makes of it opens or panics.

use p256::pkcs8::EncodePublicKey as _;
use polar_escrow::{EscrowError, EscrowedSecret, Suite};
use proptest::prelude::*;
use rsa::RsaPrivateKey;
use std::sync::OnceLock;

/// One RSA key: making one takes long in a test build. P-256 keys are made per case.
fn rsa_key() -> &'static (RsaPrivateKey, String) {
    use rsa::pkcs8::EncodePublicKey;
    static KEY: OnceLock<(RsaPrivateKey, String)> = OnceLock::new();
    KEY.get_or_init(|| {
        let private = RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap();
        let pem = private
            .to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();
        (private, pem)
    })
}

fn p256_key(seed: [u8; 32]) -> (p256::SecretKey, String) {
    // A seed that is not a valid scalar is vanishingly rare; fall back to a fixed one rather than reject the case.
    let private = p256::SecretKey::from_bytes(&seed.into())
        .unwrap_or_else(|_| p256::SecretKey::from_bytes(&[7u8; 32].into()).unwrap());
    let pem = private
        .public_key()
        .to_public_key_pem(p256::pkcs8::LineEnding::LF)
        .unwrap();
    (private, pem)
}

/// Opens `secret` with whichever private key matches its suite.
fn open(
    secret: &EscrowedSecret,
    p256: &p256::SecretKey,
    context: &[u8],
) -> Result<zeroize::Zeroizing<Vec<u8>>, EscrowError> {
    match secret.suite() {
        Suite::RsaOaep => secret.decrypt_bound(&rsa_key().0, context),
        Suite::HpkeP256 => secret.decrypt_p256_bound(p256, context),
    }
}

prop_compose! {
    fn a_case()(
        rsa in any::<bool>(),
        seed in any::<[u8; 32]>(),
        payload in proptest::collection::vec(any::<u8>(), 0..2048),
        context in proptest::collection::vec(any::<u8>(), 0..64),
    ) -> (bool, [u8; 32], Vec<u8>, Vec<u8>) {
        (rsa, seed, payload, context)
    }
}

proptest! {
    #[test]
    fn what_is_sealed_opens_to_what_was_sealed_under_the_suite_its_key_decides(
        (rsa, seed, payload, context) in a_case()
    ) {
        let (p256, p256_pem) = p256_key(seed);
        let pem = if rsa { rsa_key().1.clone() } else { p256_pem };
        let secret = EscrowedSecret::encrypt_bound(&payload, &pem, &context).unwrap();
        prop_assert_eq!(secret.suite(), if rsa { Suite::RsaOaep } else { Suite::HpkeP256 });
        let opened = open(&secret, &p256, &context).unwrap();
        prop_assert_eq!(opened.as_slice(), payload.as_slice());
        prop_assert_eq!(secret.encrypted_to_pem(), pem.as_str());
    }

    #[test]
    fn a_secret_opens_under_no_other_context(
        (rsa, seed, payload, context) in a_case(),
        other in proptest::collection::vec(any::<u8>(), 0..64),
    ) {
        prop_assume!(other != context);
        let (p256, p256_pem) = p256_key(seed);
        let pem = if rsa { rsa_key().1.clone() } else { p256_pem };
        let secret = EscrowedSecret::encrypt_bound(&payload, &pem, &context).unwrap();
        prop_assert!(matches!(open(&secret, &p256, &other), Err(EscrowError::Decryption)));
    }

    #[test]
    fn a_p256_secret_opens_under_no_other_p256_key(
        seed in any::<[u8; 32]>(),
        other_seed in any::<[u8; 32]>(),
        payload in proptest::collection::vec(any::<u8>(), 0..256),
    ) {
        let (_, pem) = p256_key(seed);
        let (other, other_pem) = p256_key(other_seed);
        prop_assume!(other_pem != pem);
        let secret = EscrowedSecret::encrypt(&payload, &pem).unwrap();
        prop_assert!(secret.decrypt_p256(&other).is_err());
    }

    #[test]
    fn a_secret_opens_with_no_key_of_the_other_suite((rsa, seed, payload, context) in a_case()) {
        let (p256, p256_pem) = p256_key(seed);
        let pem = if rsa { rsa_key().1.clone() } else { p256_pem };
        let secret = EscrowedSecret::encrypt_bound(&payload, &pem, &context).unwrap();
        let refused = if rsa {
            secret.decrypt_p256_bound(&p256, &context)
        } else {
            secret.decrypt_bound(&rsa_key().0, &context)
        };
        let wrong_suite = matches!(refused, Err(EscrowError::WrongSuite { .. }));
        prop_assert!(wrong_suite);
        // A holder that only unwraps is held to its suite too, and is never asked to unwrap.
        let holder = if rsa { Suite::HpkeP256 } else { Suite::RsaOaep };
        let mut asked = false;
        let refused = secret.decrypt_with_bound(holder, |_| { asked = true; Err("asked".into()) }, &context);
        let wrong_suite = matches!(refused, Err(EscrowError::WrongSuite { .. }));
        prop_assert!(wrong_suite);
        prop_assert!(!asked);
    }

    #[test]
    fn a_secret_with_any_byte_changed_does_not_open_and_does_not_panic(
        (rsa, seed, payload, context) in a_case(),
        at in any::<prop::sample::Index>(),
        flip in 1u8..=255,
    ) {
        let (p256, p256_pem) = p256_key(seed);
        let pem = if rsa { rsa_key().1.clone() } else { p256_pem };
        let secret = EscrowedSecret::encrypt_bound(&payload, &pem, &context).unwrap();
        let text = secret.to_document_text().unwrap();
        use base64::Engine;
        let mut bytes = base64::engine::general_purpose::STANDARD.decode(&text).unwrap();
        let i = at.index(bytes.len());
        bytes[i] ^= flip;
        let tampered = base64::engine::general_purpose::STANDARD.encode(&bytes);
        // Either it is no longer a secret at all, or it is one that does not open. Two changes leave it opening, to
        // the same payload only: a byte of the encoding's padding, which decodes to the same secret, and the stored
        // public key, which is metadata.
        if let Ok(tampered) = EscrowedSecret::from_document_text(&tampered)
            && let Ok(opened) = open(&tampered, &p256, &context)
        {
            prop_assert_eq!(opened.as_slice(), payload.as_slice());
            prop_assert!(
                tampered == secret || tampered.encrypted_to_pem() != secret.encrypted_to_pem()
            );
        }
    }

    #[test]
    fn a_wrapped_p256_key_cut_short_or_grown_is_refused_not_a_panic(
        seed in any::<[u8; 32]>(),
        cut in 0usize..113,
        extra in proptest::collection::vec(any::<u8>(), 1..16),
    ) {
        let (p256, _) = p256_key(seed);
        let secret = EscrowedSecret::encrypt(b"k", &p256_key(seed).1).unwrap();
        let wrapped = secret.wrapped_key().to_vec();
        prop_assert_eq!(wrapped.len(), polar_escrow::P256_WRAPPED_KEY_LEN);
        prop_assert!(polar_escrow::unwrap_p256(&p256, &wrapped[..cut]).is_err());
        let mut grown = wrapped.clone();
        grown.extend_from_slice(&extra);
        prop_assert!(polar_escrow::unwrap_p256(&p256, &grown).is_err());
        prop_assert_eq!(polar_escrow::unwrap_p256(&p256, &wrapped).unwrap().len(), 32);
    }

    #[test]
    fn text_that_is_not_a_secret_never_panics(text in ".{0,256}") {
        let _ = EscrowedSecret::from_document_text(&text);
    }
}
