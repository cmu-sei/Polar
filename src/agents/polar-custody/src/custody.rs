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

//! The slots and what each caller may ask of them (docs/architecture/key-custody.md, "Step 1 in detail"), without the
//! socket. `polar-init` (the installer) makes a slot's key and CSR and installs the certificate it is issued; the workload
//! signs handshakes and reads the certificate; no request gives the key out.
//!
//! A certificate is installed only if it is for the slot's own key, names the identity its CSR was made for, is valid
//! now, is no CA, and was signed by the CA the pod was given, as `polar-init` judges a bundle today
//! (`cert-client`, `judge_bundle`).

use crate::key::SlotKey;
use crate::protocol::{Refusal, Reply, Request, Slot};
use crate::tls::is_tls13_certificate_verify;
use std::collections::HashMap;
use x509_parser::prelude::*;

/// What a caller may do, by who it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Roles {
    /// `polar-init`: make keys and CSRs, install certificates.
    pub installer: bool,
    /// The workload: sign handshakes.
    pub workload: bool,
}

struct Held {
    key: SlotKey,
    identity: String,
    chain: Option<Vec<String>>,
}

pub struct Custody {
    /// The pinned CA, DER.
    ca: Vec<u8>,
    slots: HashMap<Slot, Held>,
}

/// The DER of the first certificate in `pem`.
fn der_of(pem: &str) -> Result<Vec<u8>, String> {
    x509_parser::pem::parse_x509_pem(pem.as_bytes())
        .map(|(_, block)| block.contents)
        .map_err(|e| format!("not a PEM certificate: {e}"))
}

impl Custody {
    pub fn new(pinned_ca_pem: &str) -> Result<Self, String> {
        let ca = der_of(pinned_ca_pem)?;
        X509Certificate::from_der(&ca).map_err(|e| format!("the CA does not parse: {e}"))?;
        Ok(Self {
            ca,
            slots: HashMap::new(),
        })
    }

    /// The answer to `request` from a caller with `roles`, at `now` (Unix seconds).
    pub fn handle(&mut self, roles: Roles, request: Request, now: i64) -> Reply {
        match request {
            Request::MakeCsr {
                slot,
                identity,
                names,
            } if roles.installer => self.make_csr(slot, identity, names),
            Request::Install { slot, chain_pem } if roles.installer => {
                self.install(slot, chain_pem, now)
            }
            read_only => self.handle_read(roles, read_only),
        }
    }

    /// The answer to a request that changes nothing (a signature, a certificate), which callers may have answered at
    /// once; anything else is refused here.
    pub fn handle_read(&self, roles: Roles, request: Request) -> Reply {
        match request {
            Request::Certificate { slot } if roles.installer || roles.workload => {
                match self.slots.get(&slot) {
                    None => Reply::Refused(Refusal::NoKey),
                    Some(Held { chain: None, .. }) => Reply::Refused(Refusal::NoCertificate),
                    Some(Held {
                        chain: Some(chain), ..
                    }) => Reply::Certificate(chain.clone()),
                }
            }
            Request::Sign { slot, message } if roles.workload => {
                let Some(held) = self.slots.get(&slot) else {
                    return Reply::Refused(Refusal::NoKey);
                };
                if held.chain.is_none() {
                    return Reply::Refused(Refusal::NoCertificate);
                }
                if !is_tls13_certificate_verify(&message) {
                    return Reply::Refused(Refusal::NotTls13Handshake);
                }
                Reply::Signature(held.key.sign(&message))
            }
            _ => Reply::Refused(Refusal::NotPermitted),
        }
    }

    fn make_csr(&mut self, slot: Slot, identity: String, names: Vec<String>) -> Reply {
        let key = match SlotKey::generate() {
            Ok(key) => key,
            Err(e) => return Reply::Refused(Refusal::BadRequest(e)),
        };
        let csr = match cert_issuer_common::keypair::csr_for_key(&identity, &names, &key) {
            Ok(csr) => csr,
            Err(e) => return Reply::Refused(Refusal::BadRequest(e.to_string())),
        };
        // A new key: whatever the slot held goes, its certificate with it.
        self.slots.insert(
            slot,
            Held {
                key,
                identity,
                chain: None,
            },
        );
        Reply::Csr(csr)
    }

    fn install(&mut self, slot: Slot, chain_pem: Vec<String>, now: i64) -> Reply {
        let Some(held) = self.slots.get_mut(&slot) else {
            return Reply::Refused(Refusal::NoKey);
        };
        let Some(leaf_pem) = chain_pem.first() else {
            return Reply::Refused(Refusal::BadCertificate("no certificate".to_string()));
        };
        if let Err(why) = judge(
            leaf_pem,
            &held.key.spki_der(),
            &held.identity,
            &self.ca,
            now,
        ) {
            return Reply::Refused(Refusal::BadCertificate(why));
        }
        held.chain = Some(chain_pem);
        Reply::Installed
    }
}

/// Whether `leaf_pem` is a certificate this slot may use: for `spki`, naming `identity` first, valid at `now`, no CA,
/// signed by the CA whose DER is `ca`.
fn judge(leaf_pem: &str, spki: &[u8], identity: &str, ca: &[u8], now: i64) -> Result<(), String> {
    let leaf_der = der_of(leaf_pem)?;
    let (_, leaf) =
        X509Certificate::from_der(&leaf_der).map_err(|e| format!("does not parse: {e}"))?;
    let (_, ca) =
        X509Certificate::from_der(ca).map_err(|e| format!("the CA does not parse: {e}"))?;
    if leaf.public_key().raw != spki {
        return Err("it is not for this slot's key".to_string());
    }
    let first_name = leaf
        .subject_alternative_name()
        .ok()
        .flatten()
        .and_then(|san| {
            san.value.general_names.iter().find_map(|name| match name {
                GeneralName::DNSName(dns) => Some(dns.to_string()),
                _ => None,
            })
        });
    if first_name.as_deref() != Some(identity) {
        return Err(format!("it does not name `{identity}` first"));
    }
    let at = ASN1Time::from_timestamp(now).map_err(|e| e.to_string())?;
    if !leaf.validity().is_valid_at(at) {
        return Err("it is not valid now".to_string());
    }
    if matches!(leaf.basic_constraints(), Ok(Some(bc)) if bc.value.ca) {
        return Err("it is a CA certificate".to_string());
    }
    if leaf.issuer() != ca.subject() || leaf.verify_signature(Some(ca.public_key())).is_err() {
        return Err("it was not signed by the pinned CA".to_string());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tls::{CLIENT_CONTEXT, certificate_verify_input};
    use p256::ecdsa::signature::Verifier as _;
    use p256::ecdsa::{Signature, VerifyingKey};

    pub(crate) const IDENTITY: &str = "git-repo-observer.polar.serviceaccount.cluster.local";

    pub(crate) struct TestCa {
        pub(crate) issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
        pub(crate) pem: String,
    }

    pub(crate) fn test_ca() -> TestCa {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Polar Internal CA");
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = rcgen::KeyPair::generate().unwrap();
        let pem = params.self_signed(&key).unwrap().pem();
        TestCa {
            issuer: rcgen::Issuer::new(params, key),
            pem,
        }
    }

    /// What cert-issuer does with a CSR: a client certificate for an hour from a minute ago.
    pub(crate) fn certify(ca: &TestCa, csr_pem: &str) -> String {
        let mut csr = rcgen::CertificateSigningRequestParams::from_pem(csr_pem).unwrap();
        let now = time_now();
        csr.params.not_before =
            rcgen::date_time_ymd(1970, 1, 1) + std::time::Duration::from_secs((now - 60) as u64);
        csr.params.not_after =
            rcgen::date_time_ymd(1970, 1, 1) + std::time::Duration::from_secs((now + 3600) as u64);
        csr.params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
        csr.params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
        csr.signed_by(&ca.issuer).unwrap().pem()
    }

    pub(crate) fn time_now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    const INSTALLER: Roles = Roles {
        installer: true,
        workload: false,
    };
    const WORKLOAD: Roles = Roles {
        installer: false,
        workload: true,
    };

    fn csr(custody: &mut Custody, slot: Slot) -> String {
        match custody.handle(
            INSTALLER,
            Request::MakeCsr {
                slot,
                identity: IDENTITY.to_string(),
                names: vec![],
            },
            time_now(),
        ) {
            Reply::Csr(csr) => csr,
            other => panic!("expected a CSR, got {other:?}"),
        }
    }

    /// A custody whose client slot holds a key with a certificate from `ca`, and that certificate.
    pub(crate) fn installed(ca: &TestCa) -> (Custody, String) {
        let mut custody = Custody::new(&ca.pem).unwrap();
        let csr = csr(&mut custody, Slot::Client);
        let certificate = certify(ca, &csr);
        let reply = custody.handle(
            INSTALLER,
            Request::Install {
                slot: Slot::Client,
                chain_pem: vec![certificate.clone()],
            },
            time_now(),
        );
        assert_eq!(reply, Reply::Installed);
        (custody, certificate)
    }

    fn verifying_key_of(certificate_pem: &str) -> VerifyingKey {
        let der = der_of(certificate_pem).unwrap();
        let (_, cert) = X509Certificate::from_der(&der).unwrap();
        VerifyingKey::from_sec1_bytes(&cert.public_key().subject_public_key.data).unwrap()
    }

    fn sign(custody: &Custody, roles: Roles, message: Vec<u8>) -> Reply {
        custody.handle_read(
            roles,
            Request::Sign {
                slot: Slot::Client,
                message,
            },
        )
    }

    #[test]
    fn a_handshake_signed_through_the_custody_verifies_under_the_installed_certificate() {
        let ca = test_ca();
        let (custody, certificate) = installed(&ca);
        let message = certificate_verify_input(CLIENT_CONTEXT, &[9; 32]);
        let Reply::Signature(der) = sign(&custody, WORKLOAD, message.clone()) else {
            panic!("not signed");
        };
        let signature = Signature::from_der(&der).unwrap();
        assert!(
            verifying_key_of(&certificate)
                .verify(&message, &signature)
                .is_ok()
        );
        assert_eq!(
            custody.handle_read(WORKLOAD, Request::Certificate { slot: Slot::Client }),
            Reply::Certificate(vec![certificate])
        );
    }

    #[test]
    fn only_a_tls13_handshake_is_signed() {
        let ca = test_ca();
        let (custody, _) = installed(&ca);
        for message in [
            b"anything in the agent's name".to_vec(),
            vec![b' '; 64],
            Vec::new(),
        ] {
            assert_eq!(
                sign(&custody, WORKLOAD, message),
                Reply::Refused(Refusal::NotTls13Handshake)
            );
        }
    }

    #[test]
    fn each_caller_may_ask_only_what_its_role_allows() {
        let ca = test_ca();
        let (mut custody, certificate) = installed(&ca);
        let message = certificate_verify_input(CLIENT_CONTEXT, &[9; 32]);
        let nobody = Roles::default();
        assert_eq!(
            sign(&custody, INSTALLER, message.clone()),
            Reply::Refused(Refusal::NotPermitted)
        );
        assert_eq!(
            sign(&custody, nobody, message),
            Reply::Refused(Refusal::NotPermitted)
        );
        for roles in [WORKLOAD, nobody] {
            let make = Request::MakeCsr {
                slot: Slot::Client,
                identity: IDENTITY.to_string(),
                names: vec![],
            };
            assert_eq!(
                custody.handle(roles, make, time_now()),
                Reply::Refused(Refusal::NotPermitted)
            );
            let install = Request::Install {
                slot: Slot::Client,
                chain_pem: vec![certificate.clone()],
            };
            assert_eq!(
                custody.handle(roles, install, time_now()),
                Reply::Refused(Refusal::NotPermitted)
            );
        }
        assert_eq!(
            custody.handle_read(nobody, Request::Certificate { slot: Slot::Client }),
            Reply::Refused(Refusal::NotPermitted)
        );
    }

    #[test]
    fn nothing_is_signed_before_a_certificate_is_installed_and_a_new_key_drops_the_old_certificate()
    {
        let ca = test_ca();
        let mut custody = Custody::new(&ca.pem).unwrap();
        let message = certificate_verify_input(CLIENT_CONTEXT, &[9; 32]);
        assert_eq!(
            sign(&custody, WORKLOAD, message.clone()),
            Reply::Refused(Refusal::NoKey)
        );
        csr(&mut custody, Slot::Client);
        assert_eq!(
            sign(&custody, WORKLOAD, message.clone()),
            Reply::Refused(Refusal::NoCertificate)
        );
        let (mut custody, certificate) = installed(&ca);
        csr(&mut custody, Slot::Client);
        assert_eq!(
            sign(&custody, WORKLOAD, message),
            Reply::Refused(Refusal::NoCertificate)
        );
        let reinstall = Request::Install {
            slot: Slot::Client,
            chain_pem: vec![certificate],
        };
        assert!(matches!(
            custody.handle(INSTALLER, reinstall, time_now()),
            Reply::Refused(Refusal::BadCertificate(why)) if why.contains("not for this slot's key")
        ));
    }

    #[test]
    fn a_certificate_for_another_key_another_identity_another_ca_or_out_of_date_is_refused() {
        let ca = test_ca();
        let mut custody = Custody::new(&ca.pem).unwrap();
        let ours = csr(&mut custody, Slot::Client);
        let try_install = |custody: &mut Custody, certificate: String, at: i64| {
            custody.handle(
                INSTALLER,
                Request::Install {
                    slot: Slot::Client,
                    chain_pem: vec![certificate],
                },
                at,
            )
        };
        let refused = |reply: Reply, says: &str| {
            assert!(
                matches!(&reply, Reply::Refused(Refusal::BadCertificate(why)) if why.contains(says)),
                "{says}: {reply:?}"
            )
        };
        // Another key: a CSR made elsewhere for the same identity.
        let other = cert_issuer_common::keypair::generate_csr(
            IDENTITY,
            &cert_issuer_common::keypair::KeyAlgorithm::EcdsaP256,
            &[],
        )
        .unwrap();
        refused(
            try_install(&mut custody, certify(&ca, &other.csr_pem), time_now()),
            "not for this slot's key",
        );
        // Signed by another CA.
        refused(
            try_install(&mut custody, certify(&test_ca(), &ours), time_now()),
            "pinned CA",
        );
        // Not valid then.
        refused(
            try_install(&mut custody, certify(&ca, &ours), time_now() + 7200),
            "not valid now",
        );
        // Our slot's key, certified for another identity.
        let mut renamed = rcgen::CertificateSigningRequestParams::from_pem(&ours).unwrap();
        renamed.params.subject_alt_names = vec![rcgen::SanType::DnsName(
            "cassini.polar.serviceaccount.cluster.local"
                .try_into()
                .unwrap(),
        )];
        let now = time_now();
        renamed.params.not_before =
            rcgen::date_time_ymd(1970, 1, 1) + std::time::Duration::from_secs((now - 60) as u64);
        renamed.params.not_after =
            rcgen::date_time_ymd(1970, 1, 1) + std::time::Duration::from_secs((now + 3600) as u64);
        let renamed = renamed.signed_by(&ca.issuer).unwrap().pem();
        refused(
            try_install(&mut custody, renamed, time_now()),
            "does not name",
        );
        // Our slot's key, in a CA certificate.
        let mut as_ca = rcgen::CertificateSigningRequestParams::from_pem(&ours).unwrap();
        as_ca.params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        as_ca.params.not_before =
            rcgen::date_time_ymd(1970, 1, 1) + std::time::Duration::from_secs((now - 60) as u64);
        as_ca.params.not_after =
            rcgen::date_time_ymd(1970, 1, 1) + std::time::Duration::from_secs((now + 3600) as u64);
        let as_ca = as_ca.signed_by(&ca.issuer).unwrap().pem();
        refused(
            try_install(&mut custody, as_ca, time_now()),
            "CA certificate",
        );
        // And the right one is taken.
        assert_eq!(
            try_install(&mut custody, certify(&ca, &ours), time_now()),
            Reply::Installed
        );
    }

    mod props {
        use super::*;
        use crate::tls::SERVER_CONTEXT;
        use proptest::prelude::*;

        fn context() -> impl Strategy<Value = &'static [u8]> {
            prop_oneof![Just(CLIENT_CONTEXT), Just(SERVER_CONTEXT)]
        }

        fn hash() -> impl Strategy<Value = Vec<u8>> {
            prop_oneof![
                prop::collection::vec(any::<u8>(), 32),
                prop::collection::vec(any::<u8>(), 48)
            ]
        }

        /// Any request a caller could send, with this slot's identity or another.
        fn request() -> impl Strategy<Value = Request> {
            let slot = prop_oneof![Just(Slot::Client), Just(Slot::Server)];
            prop_oneof![
                (slot.clone(), "[a-z.-]{1,30}").prop_map(|(slot, identity)| Request::MakeCsr {
                    slot,
                    identity,
                    names: vec![]
                }),
                slot.clone().prop_map(|slot| Request::Certificate { slot }),
                (slot.clone(), prop::collection::vec(any::<u8>(), 0..160))
                    .prop_map(|(slot, message)| Request::Sign { slot, message }),
                (slot, "[ -~]{0,40}").prop_map(|(slot, pem)| Request::Install {
                    slot,
                    chain_pem: vec![pem]
                }),
            ]
        }

        fn ca() -> &'static TestCa {
            static CA: std::sync::OnceLock<TestCa> = std::sync::OnceLock::new();
            CA.get_or_init(test_ca)
        }

        proptest! {
            #[test]
            fn every_tls13_handshake_it_signs_verifies_under_its_certificate(context in context(), hash in hash()) {
                let (custody, certificate) = installed(ca());
                let message = certificate_verify_input(context, &hash);
                let Reply::Signature(der) = sign(&custody, WORKLOAD, message.clone()) else {
                    return Err(TestCaseError::fail("not signed"));
                };
                let signature = Signature::from_der(&der).unwrap();
                prop_assert!(verifying_key_of(&certificate).verify(&message, &signature).is_ok());
            }

            #[test]
            fn anything_that_is_not_a_tls13_handshake_is_never_signed(message in prop::collection::vec(any::<u8>(), 0..200)) {
                prop_assume!(!is_tls13_certificate_verify(&message));
                let (custody, _) = installed(ca());
                prop_assert_eq!(sign(&custody, WORKLOAD, message), Reply::Refused(Refusal::NotTls13Handshake));
            }

            #[test]
            fn no_request_from_any_caller_gets_a_key_out(requests in prop::collection::vec((request(), any::<bool>(), any::<bool>()), 0..12)) {
                let (mut custody, _) = installed(ca());
                for (request, installer, workload) in requests {
                    let reply = custody.handle(Roles { installer, workload }, request, time_now());
                    let bytes = format!("{reply:?}").into_bytes();
                    let encoded = rkyv::to_bytes::<rkyv::rancor::Error>(&reply).unwrap().to_vec();
                    for held in custody.slots.values() {
                        let secret = held.key.secret_bytes();
                        prop_assert!(!encoded.windows(secret.len()).any(|w| w == secret.as_slice()));
                        let hex: String = secret.iter().map(|b| format!("{b:02x}")).collect();
                        prop_assert!(!String::from_utf8_lossy(&bytes).contains(&hex));
                    }
                }
            }
        }
    }
}
