//! Signing and verifying, against real certificates.

use cert_issuer_common::capability::Capability;
use polar_signing::document::{Algorithm, signing_input};
use polar_signing::testing::{self as support, Kind, Spec, now, root, signer};
use polar_signing::{
    Authority, Document, LayoutError, RequireCapability, SignError, Trust, VerifyError, sign,
    verify,
};
use rcgen::KeyUsagePurpose;
use serde_json::{Value, json};

fn payload() -> Value {
    json!({
        "id": "hello-agent",
        "target": {"agent": "hello-agent"},
        "version": 7,
        "changes": [{"name": "GREETING", "value": "hello"}]
    })
}

fn trust(pem: &str) -> Trust {
    Trust::from_pem(&[pem.to_string()]).unwrap()
}

/// A document signed by a default signer under `ca`, with its chain.
fn signed(ca: &support::Authority, spec: Spec) -> (Document, support::Signer) {
    let s = signer(ca, spec);
    let document = sign(payload(), std::slice::from_ref(&s.cert_pem), &s.key_pem).unwrap();
    (document, s)
}

#[test]
fn a_signature_verifies_for_either_key_type_and_says_who_signed() {
    let ca = root("Polar Internal CA");
    for kind in [Kind::P256, Kind::Ed25519] {
        let (document, _) = signed(
            &ca,
            Spec {
                kind,
                ..Spec::default()
            },
        );
        let who = verify(&document, &trust(&ca.pem), now()).unwrap();
        assert_eq!(who.username.as_deref(), Some("alice"));
        assert_eq!(who.email.as_deref(), Some("alice@example.org"));
        assert_eq!(
            who.identity.as_deref(),
            Some("d983a3d2-9b61-41c5-88a0-d123fb50ba33")
        );
        assert_eq!(who.capabilities, vec![Capability::UpdateConfiguration]);
        assert!(!who.serial.is_empty());
        assert_eq!(
            document.signature.algorithm,
            if kind == Kind::P256 {
                Algorithm::EcdsaP256Sha256
            } else {
                Algorithm::Ed25519
            }
        );
    }
}

#[test]
fn signing_is_deterministic() {
    let ca = root("Polar Internal CA");
    for kind in [Kind::P256, Kind::Ed25519] {
        let s = signer(
            &ca,
            Spec {
                kind,
                ..Spec::default()
            },
        );
        let one = sign(payload(), std::slice::from_ref(&s.cert_pem), &s.key_pem).unwrap();
        let two = sign(payload(), std::slice::from_ref(&s.cert_pem), &s.key_pem).unwrap();
        assert_eq!(one, two);
    }
}

#[test]
fn a_document_survives_the_json_it_travels_as() {
    let ca = root("Polar Internal CA");
    let (document, _) = signed(&ca, Spec::default());
    let text = serde_json::to_string(&document.to_value()).unwrap();
    let back = Document::from_value(&serde_json::from_str(&text).unwrap()).unwrap();
    assert_eq!(back, document);
    verify(&back, &trust(&ca.pem), now()).unwrap();
}

#[test]
fn the_order_the_payload_was_written_in_does_not_matter() {
    let ca = root("Polar Internal CA");
    let s = signer(&ca, Spec::default());
    let document = sign(payload(), std::slice::from_ref(&s.cert_pem), &s.key_pem).unwrap();
    let mut shuffled = document.to_value();
    let reversed: serde_json::Map<String, Value> = document
        .payload
        .as_object()
        .unwrap()
        .iter()
        .rev()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    shuffled["payload"] = Value::Object(reversed);
    let reread = Document::from_value(&shuffled).unwrap();
    verify(&reread, &trust(&ca.pem), now()).unwrap();
}

#[test]
fn any_change_to_the_payload_breaks_the_signature() {
    let ca = root("Polar Internal CA");
    let (document, _) = signed(&ca, Spec::default());
    let trust = trust(&ca.pem);
    for edit in [
        |p: &mut Value| p["version"] = json!(8),
        |p: &mut Value| p["target"]["agent"] = json!("another-agent"),
        |p: &mut Value| p["changes"][0]["value"] = json!("goodbye"),
        |p: &mut Value| p["changes"][0]["extra"] = json!(1),
        |p: &mut Value| {
            p.as_object_mut().unwrap().remove("id");
        },
    ] {
        let mut changed = document.clone();
        edit(&mut changed.payload);
        assert_eq!(
            verify(&changed, &trust, now()),
            Err(VerifyError::BadSignature)
        );
    }
}

#[test]
fn a_changed_signature_or_one_from_another_document_is_refused() {
    let ca = root("Polar Internal CA");
    let s = signer(&ca, Spec::default());
    let one = sign(payload(), std::slice::from_ref(&s.cert_pem), &s.key_pem).unwrap();
    let mut other_payload = payload();
    other_payload["version"] = json!(8);
    let two = sign(other_payload, std::slice::from_ref(&s.cert_pem), &s.key_pem).unwrap();
    let trust = trust(&ca.pem);

    let mut swapped = one.clone();
    swapped.signature.value = two.signature.value.clone();
    assert_eq!(
        verify(&swapped, &trust, now()),
        Err(VerifyError::BadSignature)
    );

    let mut flipped = one.clone();
    flipped.signature.value[0] ^= 0xff;
    assert_eq!(
        verify(&flipped, &trust, now()),
        Err(VerifyError::BadSignature)
    );

    let mut short = one;
    short.signature.value.truncate(10);
    assert_eq!(
        verify(&short, &trust, now()),
        Err(VerifyError::BadSignature)
    );
}

#[test]
fn a_signature_made_without_the_domain_line_is_refused() {
    use p256::ecdsa::signature::Signer as _;
    use p256::pkcs8::DecodePrivateKey as _;
    let ca = root("Polar Internal CA");
    let s = signer(&ca, Spec::default());
    let mut document = sign(payload(), std::slice::from_ref(&s.cert_pem), &s.key_pem).unwrap();

    // The same canonical payload, signed with the same key, but not for this purpose.
    let key = p256::ecdsa::SigningKey::from_pkcs8_pem(&s.key_pem).unwrap();
    let bare = polar_signing::canonical::encode(&document.payload).unwrap();
    let signature: p256::ecdsa::Signature = key.sign(&bare);
    document.signature.value = signature.to_bytes().to_vec();
    assert_eq!(
        verify(&document, &trust(&ca.pem), now()),
        Err(VerifyError::BadSignature)
    );
    // And the input the system does sign starts with the line.
    assert!(
        signing_input(&document.payload)
            .unwrap()
            .starts_with(b"polar-config-document-v1\n")
    );
}

#[test]
fn a_key_that_is_not_the_certificates_cannot_sign() {
    let ca = root("Polar Internal CA");
    let one = signer(&ca, Spec::default());
    let two = signer(&ca, Spec::default());
    assert_eq!(
        sign(payload(), &[one.cert_pem], &two.key_pem),
        Err(SignError::KeyCertificateMismatch)
    );
    let ed = signer(
        &ca,
        Spec {
            kind: Kind::Ed25519,
            ..Spec::default()
        },
    );
    let ed2 = signer(
        &ca,
        Spec {
            kind: Kind::Ed25519,
            ..Spec::default()
        },
    );
    assert_eq!(
        sign(payload(), &[ed.cert_pem], &ed2.key_pem),
        Err(SignError::KeyCertificateMismatch)
    );
}

#[test]
fn only_an_object_with_integers_can_be_signed() {
    let ca = root("Polar Internal CA");
    let s = signer(&ca, Spec::default());
    assert_eq!(
        sign(json!([1]), std::slice::from_ref(&s.cert_pem), &s.key_pem),
        Err(SignError::PayloadNotAnObject)
    );
    assert!(matches!(
        sign(
            json!({"ratio": 1.5}),
            std::slice::from_ref(&s.cert_pem),
            &s.key_pem
        ),
        Err(SignError::Canonical(_))
    ));
    assert_eq!(sign(payload(), &[], &s.key_pem), Err(SignError::EmptyChain));
}

#[test]
fn a_certificate_from_another_ca_is_not_trusted() {
    let real = root("Polar Internal CA");
    let rogue = root("Rogue CA");
    let (document, _) = signed(&rogue, Spec::default());
    assert_eq!(
        verify(&document, &trust(&real.pem), now()),
        Err(VerifyError::UntrustedRoot)
    );
}

#[test]
fn a_ca_with_the_same_name_but_another_key_is_not_trusted() {
    let real = root("Polar Internal CA");
    let impostor = root("Polar Internal CA");
    let (document, _) = signed(&impostor, Spec::default());
    assert_eq!(
        verify(&document, &trust(&real.pem), now()),
        Err(VerifyError::UntrustedRoot)
    );
}

#[test]
fn an_expired_or_not_yet_valid_signing_certificate_is_refused() {
    let ca = root("Polar Internal CA");
    let trust = trust(&ca.pem);
    let (expired, _) = signed(
        &ca,
        Spec {
            not_before_year: 2026,
            not_after_year: 2026, // ends 2026-01-01, before now
            ..Spec::default()
        },
    );
    assert!(matches!(
        verify(&expired, &trust, now()),
        Err(VerifyError::NotValidNow(_))
    ));
    let (future, _) = signed(
        &ca,
        Spec {
            not_before_year: 2028,
            not_after_year: 2030,
            ..Spec::default()
        },
    );
    assert!(matches!(
        verify(&future, &trust, now()),
        Err(VerifyError::NotValidNow(_))
    ));
    // The same certificate is fine at a time inside its life.
    let at_2029 = support::now() + 3 * 365 * 24 * 3600;
    assert!(verify(&future, &trust, at_2029).is_ok());
}

#[test]
fn a_certificate_that_does_not_permit_signatures_is_refused() {
    let ca = root("Polar Internal CA");
    let trust = trust(&ca.pem);
    let (encipherment_only, _) = signed(
        &ca,
        Spec {
            key_usages: vec![KeyUsagePurpose::KeyEncipherment],
            ..Spec::default()
        },
    );
    assert_eq!(
        verify(&encipherment_only, &trust, now()),
        Err(VerifyError::KeyUsage)
    );
    let (no_usage, _) = signed(
        &ca,
        Spec {
            key_usages: vec![],
            ..Spec::default()
        },
    );
    assert_eq!(verify(&no_usage, &trust, now()), Err(VerifyError::KeyUsage));
}

#[test]
fn the_algorithm_the_document_claims_must_be_the_certificates() {
    let ca = root("Polar Internal CA");
    let (mut document, _) = signed(&ca, Spec::default());
    document.signature.algorithm = Algorithm::Ed25519;
    assert_eq!(
        verify(&document, &trust(&ca.pem), now()),
        Err(VerifyError::AlgorithmMismatch {
            claimed: "ed25519",
            actual: "ecdsa-p256-sha256"
        })
    );
}

#[test]
fn a_chain_through_an_intermediate_verifies_and_the_root_may_be_carried_or_not() {
    let ca = root("Polar Internal CA");
    let mid = support::intermediate(&ca, "Polar Issuing CA", 2030);
    let s = signer(&mid, Spec::default());
    let trust = trust(&ca.pem);

    let without_root = sign(
        payload(),
        &[s.cert_pem.clone(), mid.pem.clone()],
        &s.key_pem,
    )
    .unwrap();
    verify(&without_root, &trust, now()).unwrap();

    let with_root = sign(
        payload(),
        &[s.cert_pem.clone(), mid.pem.clone(), ca.pem.clone()],
        &s.key_pem,
    )
    .unwrap();
    verify(&with_root, &trust, now()).unwrap();

    // Leaving the intermediate out leaves nothing that links the signer to the CA.
    let missing = sign(payload(), std::slice::from_ref(&s.cert_pem), &s.key_pem).unwrap();
    assert_eq!(
        verify(&missing, &trust, now()),
        Err(VerifyError::UntrustedRoot)
    );
}

#[test]
fn an_intermediate_that_has_expired_or_is_not_a_ca_breaks_the_chain() {
    let ca = root("Polar Internal CA");
    let trust = trust(&ca.pem);

    let old_mid = support::intermediate(&ca, "Old Issuing CA", 2026); // ended 2026-01-01
    let s = signer(&old_mid, Spec::default());
    let doc = sign(
        payload(),
        &[s.cert_pem.clone(), old_mid.pem.clone()],
        &s.key_pem,
    )
    .unwrap();
    assert!(matches!(
        verify(&doc, &trust, now()),
        Err(VerifyError::NotValidNow(_))
    ));

    // An end-entity certificate is not a CA, whatever it is used to sign.
    let plain = signer(&ca, Spec::default());
    let issuer = support::Authority {
        issuer: rcgen::Issuer::from_ca_cert_pem(
            &plain.cert_pem,
            rcgen::KeyPair::from_pem(&plain.key_pem).unwrap(),
        )
        .unwrap(),
        pem: plain.cert_pem.clone(),
    };
    let under_plain = signer(&issuer, Spec::default());
    let doc = sign(
        payload(),
        &[under_plain.cert_pem.clone(), plain.cert_pem.clone()],
        &under_plain.key_pem,
    )
    .unwrap();
    assert!(matches!(
        verify(&doc, &trust, now()),
        Err(VerifyError::IssuerNotCa(_))
    ));
}

#[test]
fn an_over_long_chain_is_refused() {
    let ca = root("Polar Internal CA");
    let s = signer(&ca, Spec::default());
    let mut document = sign(payload(), std::slice::from_ref(&s.cert_pem), &s.key_pem).unwrap();
    document.signature.certificate_chain = vec![s.cert_pem.clone(); 6];
    assert!(matches!(
        verify(&document, &trust(&ca.pem), now()),
        Err(VerifyError::ChainTooLong(6))
    ));
}

#[test]
fn a_chain_that_is_not_certificates_is_refused() {
    let ca = root("Polar Internal CA");
    let (mut document, _) = signed(&ca, Spec::default());
    document.signature.certificate_chain = vec!["not a certificate".to_string()];
    assert!(matches!(
        verify(&document, &trust(&ca.pem), now()),
        Err(VerifyError::BadCertificate(_))
    ));
}

#[test]
fn a_signer_with_the_update_configuration_capability_is_authorised_and_one_without_is_not() {
    let ca = root("Polar Internal CA");
    let trust = trust(&ca.pem);
    let rule = RequireCapability::update_configuration();

    let (allowed, _) = signed(&ca, Spec::default());
    let who = verify(&allowed, &trust, now()).unwrap();
    assert!(rule.authorize(&who, &allowed.payload).is_ok());

    let (denied, _) = signed(
        &ca,
        Spec {
            capabilities: vec!["c:grant"],
            ..Spec::default()
        },
    );
    let who = verify(&denied, &trust, now()).unwrap();
    let refusal = rule.authorize(&who, &denied.payload).unwrap_err();
    assert!(refusal.0.contains("alice"), "{refusal}");
    assert!(refusal.0.contains("c:update-configuration"), "{refusal}");

    let (none, _) = signed(
        &ca,
        Spec {
            capabilities: vec![],
            ..Spec::default()
        },
    );
    let who = verify(&none, &trust, now()).unwrap();
    assert!(rule.authorize(&who, &none.payload).is_err());
}

#[test]
fn a_document_with_a_field_the_signature_does_not_cover_is_refused_before_anything_else() {
    let ca = root("Polar Internal CA");
    let (document, _) = signed(&ca, Spec::default());
    let mut value = document.to_value();
    value["owner"] = json!("someone");
    assert_eq!(
        Document::from_value(&value),
        Err(LayoutError::UnexpectedField("owner".into()))
    );
}

#[test]
fn running_a_command_needs_its_own_capability_and_update_policy_does_not_give_it() {
    let ca = root("Polar Internal CA");
    let trust = trust(&ca.pem);
    let rule = RequireCapability::run_command();

    let (allowed, _) = signed(
        &ca,
        Spec {
            capabilities: vec!["c:run-command"],
            ..Spec::default()
        },
    );
    let who = verify(&allowed, &trust, now()).unwrap();
    assert_eq!(who.capabilities, vec![Capability::RunCommand]);
    assert!(rule.authorize(&who, &allowed.payload).is_ok());

    // The other capabilities, all together, are not it.
    let (others, _) = signed(
        &ca,
        Spec {
            capabilities: vec![
                "c:update-policy",
                "c:update-configuration",
                "c:grant",
                "c:revoke",
                "c:provision-account",
            ],
            ..Spec::default()
        },
    );
    let who = verify(&others, &trust, now()).unwrap();
    let refusal = rule.authorize(&who, &others.payload).unwrap_err();
    assert!(refusal.0.contains("c:run-command"), "{refusal}");

    // And it does not stand in for update-policy or update-configuration either.
    let (only_run, _) = signed(
        &ca,
        Spec {
            capabilities: vec!["c:run-command"],
            ..Spec::default()
        },
    );
    let who = verify(&only_run, &trust, now()).unwrap();
    assert!(
        RequireCapability::update_policy()
            .authorize(&who, &only_run.payload)
            .is_err()
    );
    assert!(
        RequireCapability::update_configuration()
            .authorize(&who, &only_run.payload)
            .is_err()
    );
}

#[test]
fn changing_configuration_and_changing_policy_are_separate_privileges() {
    let ca = root("Polar Internal CA");
    let trust = trust(&ca.pem);

    // A holder of update-policy alone may not change configuration: a repository that holds no Cedar policy
    // updates needs no such privilege.
    let (policy_only, _) = signed(
        &ca,
        Spec {
            capabilities: vec!["c:update-policy"],
            ..Spec::default()
        },
    );
    let who = verify(&policy_only, &trust, now()).unwrap();
    assert!(
        RequireCapability::update_policy()
            .authorize(&who, &policy_only.payload)
            .is_ok()
    );
    assert!(
        RequireCapability::update_configuration()
            .authorize(&who, &policy_only.payload)
            .is_err()
    );

    // And the other way round.
    let (config_only, _) = signed(&ca, Spec::default());
    let who = verify(&config_only, &trust, now()).unwrap();
    assert!(
        RequireCapability::update_configuration()
            .authorize(&who, &config_only.payload)
            .is_ok()
    );
    assert!(
        RequireCapability::update_policy()
            .authorize(&who, &config_only.payload)
            .is_err()
    );
}
