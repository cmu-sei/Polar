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
//! The custodian (docs/architecture/key-custody.md, "Part A's bootstrap: the custodian"): `polar-init` installs only
//! the custodian's own certificate, bought with the pod's token, and says what the custodian is to fetch
//! (`Request::Bootstrap`); the custodian fills the workload's slots and the escrow key itself, through the operations
//! below that no request reaches. The custodian's key signs only the daemon's own handshake; the escrow key only
//! unwraps.
//!
//! A certificate is installed only if it is for the slot's own key, names the identity its CSR was made for, is valid
//! now, is no CA, and was signed by the CA the pod was given, as `polar-init` judges a bundle today
//! (`cert-client`, `judge_bundle`).

use crate::escrow::EscrowKey;
use crate::key::SlotKey;
use crate::protocol::{Refusal, Reply, Request, Slot};
use crate::tls::is_tls13_certificate_verify;
use cert_issuer_common::custody::{CustodyPermission, custodian_of, is_reserved};
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

/// What the custodian is to fetch for the workload (`Request::Bootstrap`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The workload's identity: the custodian's is `custody.<identity>`.
    pub identity: String,
    pub client: bool,
    /// The server certificate's names, if it is to have one.
    pub server: Option<Vec<String>>,
    pub escrow: bool,
}

pub struct Custody {
    /// The pinned CA, DER.
    ca: Vec<u8>,
    slots: HashMap<Slot, Held>,
    /// What the custodian is to fetch, once `polar-init` has said.
    plan: Option<Plan>,
    /// An escrow key made for issuance, not yet escrowed.
    pending_escrow: Option<EscrowKey>,
    /// The pod's escrow key, which only unwraps.
    escrow: Option<EscrowKey>,
    /// The custodian has fetched everything the plan says, or could not.
    ready: bool,
    failed: Option<String>,
    /// A certificate it held was revoked: its keys are gone, and it refuses everything.
    revoked: bool,
}

// No key, no certificate: only whether it is revoked.
impl std::fmt::Debug for Custody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Custody")
            .field("revoked", &self.is_revoked())
            .finish_non_exhaustive()
    }
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
            plan: None,
            pending_escrow: None,
            escrow: None,
            ready: false,
            failed: None,
            revoked: false,
        })
    }

    /// The serial of the certificate installed in `slot`, as the credentials agent's ledger writes it.
    pub fn serial(&self, slot: Slot) -> Option<String> {
        let leaf = self.slots.get(&slot)?.chain.as_ref()?.first()?;
        cert_issuer_common::serial::serial_from_pem(leaf)
    }

    /// The certificate chain installed in `slot`, PEM.
    pub fn chain(&self, slot: Slot) -> Option<Vec<String>> {
        self.slots.get(&slot)?.chain.clone()
    }

    /// The credentials agent says `serials` are revoked (docs/architecture/key-custody.md, step 5). If any is a
    /// certificate installed here, every key is destroyed (the slots go, and each key is zeroed as it goes), and from
    /// then on every request is refused: the daemon is to exit. Returns the slots whose certificates were revoked.
    pub fn revoke(&mut self, serials: &[String]) -> Vec<Slot> {
        let mut revoked: Vec<Slot> = self
            .slots
            .keys()
            .copied()
            .filter(|slot| {
                self.serial(*slot)
                    .is_some_and(|serial| serials.contains(&serial))
            })
            .collect();
        revoked.sort_by_key(|slot| matches!(slot, Slot::Server));
        if !revoked.is_empty() {
            self.slots.clear();
            self.pending_escrow = None;
            self.escrow = None;
            self.revoked = true;
        }
        revoked
    }

    /// Whether a certificate it held has been revoked.
    pub fn is_revoked(&self) -> bool {
        self.revoked
    }

    /// The answer to `request` from a caller with `roles`, at `now` (Unix seconds).
    pub fn handle(&mut self, roles: Roles, request: Request, now: i64) -> Reply {
        if self.revoked {
            return Reply::Refused(Refusal::Revoked);
        }
        match request {
            // A slot is sealed once a certificate is installed in it. In a pod polar-init and the workload are one
            // UID, so the roles are kept apart in time: the workload runs after polar-init has installed every
            // certificate it uses (docs/architecture/key-custody.md, step 4, item 1).
            Request::MakeCsr { slot, .. } | Request::Install { slot, .. }
                if roles.installer && self.is_sealed(slot) =>
            {
                Reply::Refused(Refusal::Sealed)
            }
            // Once the custodian has its plan, the workload's slots are its own to fill.
            Request::MakeCsr { slot, .. } | Request::Install { slot, .. }
                if roles.installer && slot != Slot::Custodian && self.plan.is_some() =>
            {
                Reply::Refused(Refusal::Sealed)
            }
            // The custodians' space is the custodian slot's, and only that slot's.
            Request::MakeCsr {
                slot, ref identity, ..
            } if roles.installer && (slot == Slot::Custodian) != is_reserved(identity) => {
                Reply::Refused(Refusal::BadRequest(
                    "the custodian slot is for a custodian's identity, and only it is".to_string(),
                ))
            }
            Request::Bootstrap {
                identity,
                client,
                server,
                escrow,
            } if roles.installer => self.bootstrap(Plan {
                identity,
                client,
                server,
                escrow,
            }),
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

    fn is_sealed(&self, slot: Slot) -> bool {
        self.slots
            .get(&slot)
            .is_some_and(|held| held.chain.is_some())
    }

    /// The answer to a request that changes nothing (a signature, a certificate), which callers may have answered at
    /// once; anything else is refused here.
    pub fn handle_read(&self, roles: Roles, request: Request) -> Reply {
        if self.revoked {
            return Reply::Refused(Refusal::Revoked);
        }
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
            // The custodian's key signs the daemon's own handshake, in process (`sign_own`), and no caller's.
            Request::Sign {
                slot: Slot::Custodian,
                ..
            } => Reply::Refused(Refusal::NotPermitted),
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
            Request::Ready if roles.installer => match (&self.failed, self.ready) {
                (Some(why), _) => Reply::Refused(Refusal::BootstrapFailed(why.clone())),
                (None, true) => Reply::Ready,
                (None, false) => Reply::Refused(Refusal::NotReady),
            },
            Request::Unwrap { wrapped } if roles.workload => match &self.escrow {
                None => Reply::Refused(Refusal::NoKey),
                Some(key) => match key.unwrap(&wrapped) {
                    Ok(unwrapped) => Reply::Unwrapped(unwrapped.to_vec()),
                    Err(_) => Reply::Refused(Refusal::BadRequest(
                        "not a key wrapped to this pod's escrow key".to_string(),
                    )),
                },
            },
            _ => Reply::Refused(Refusal::NotPermitted),
        }
    }

    /// The plan `polar-init` gives, once the custodian's certificate is installed: only what that certificate names,
    /// for the workload it is the custodian of, and once.
    fn bootstrap(&mut self, plan: Plan) -> Reply {
        if self.plan.is_some() {
            return Reply::Refused(Refusal::Sealed);
        }
        let Some(Held {
            identity: custodian,
            chain: Some(chain),
            ..
        }) = self.slots.get(&Slot::Custodian)
        else {
            return Reply::Refused(Refusal::NoCertificate);
        };
        if custodian_of(custodian) != Some(plan.identity.as_str()) || is_reserved(&plan.identity) {
            return Reply::Refused(Refusal::BadRequest(format!(
                "`{}` is not the workload this custodian keeps keys for",
                plan.identity
            )));
        }
        let names = chain
            .first()
            .and_then(|leaf| cert_issuer_common::custody::extract_custody_permissions(leaf).ok())
            .unwrap_or_default();
        for (wanted, permission) in [
            (plan.client, CustodyPermission::Client),
            (plan.server.is_some(), CustodyPermission::Server),
            (plan.escrow, CustodyPermission::Escrow),
        ] {
            if wanted && !names.contains(&permission) {
                return Reply::Refused(Refusal::BadRequest(format!(
                    "the custodian's certificate does not name {}",
                    permission.to_san_string()
                )));
            }
        }
        self.ready = !plan.client && plan.server.is_none() && !plan.escrow;
        self.plan = Some(plan);
        Reply::Installed
    }

    // ---- the custodian's own operations: no request reaches these ------------------------------------------------

    /// What the custodian is to fetch, once `polar-init` has said.
    pub fn plan(&self) -> Option<Plan> {
        self.plan.clone()
    }

    /// Whether the plan is fetched.
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Everything the plan says is fetched.
    pub fn set_ready(&mut self) {
        if !self.revoked && self.failed.is_none() {
            self.ready = true;
        }
    }

    /// The custodian could not fetch what it was asked to, and will not: `polar-init` is told, and the pod starts
    /// again.
    pub fn set_failed(&mut self, why: String) {
        if !self.ready {
            self.failed = Some(why);
        }
    }

    /// A new key for one of the workload's slots, and its CSR, for the custodian to ask its certificate with.
    pub fn workload_csr(&mut self, slot: Slot, names: Vec<String>) -> Result<String, Refusal> {
        let identity = match (&self.plan, slot) {
            (_, Slot::Custodian) => return Err(Refusal::NotPermitted),
            (Some(plan), _) => plan.identity.clone(),
            (None, _) => return Err(Refusal::NotPermitted),
        };
        if self.revoked {
            return Err(Refusal::Revoked);
        }
        if self.is_sealed(slot) {
            return Err(Refusal::Sealed);
        }
        match self.make_csr(slot, identity, names) {
            Reply::Csr(csr) => Ok(csr),
            Reply::Refused(refusal) => Err(refusal),
            other => Err(Refusal::BadRequest(format!("unexpected: {other:?}"))),
        }
    }

    /// The certificate the custodian was issued for one of the workload's slots, judged as `polar-init`'s would be.
    pub fn install_workload(
        &mut self,
        slot: Slot,
        chain_pem: Vec<String>,
        now: i64,
    ) -> Result<(), Refusal> {
        if slot == Slot::Custodian || self.plan.is_none() {
            return Err(Refusal::NotPermitted);
        }
        if self.revoked {
            return Err(Refusal::Revoked);
        }
        if self.is_sealed(slot) {
            return Err(Refusal::Sealed);
        }
        match self.install(slot, chain_pem, now) {
            Reply::Installed => Ok(()),
            Reply::Refused(refusal) => Err(refusal),
            other => Err(Refusal::BadRequest(format!("unexpected: {other:?}"))),
        }
    }

    /// A new escrow key, when nothing is escrowed for the workload, and its CSR, naming the workload.
    pub fn escrow_csr(&mut self) -> Result<String, Refusal> {
        let identity = self
            .plan
            .as_ref()
            .filter(|plan| plan.escrow)
            .map(|plan| plan.identity.clone())
            .ok_or(Refusal::NotPermitted)?;
        if self.revoked {
            return Err(Refusal::Revoked);
        }
        if self.escrow.is_some() {
            return Err(Refusal::Sealed);
        }
        let key = EscrowKey::generate().map_err(Refusal::BadRequest)?;
        let csr = key.csr(&identity).map_err(Refusal::BadRequest)?;
        self.pending_escrow = Some(key);
        Ok(csr)
    }

    /// The new escrow key, sealed to `recipient`, the credentials agent's key the daemon pinned under the CA: the one
    /// way the key leaves. Never to a key a caller names.
    pub fn sealed_escrow(
        &self,
        recipient: &rsa::RsaPublicKey,
    ) -> Result<polar_escrow::EscrowedSecret, Refusal> {
        if self.revoked {
            return Err(Refusal::Revoked);
        }
        self.pending_escrow
            .as_ref()
            .ok_or(Refusal::NoKey)?
            .sealed_to(recipient)
            .map_err(Refusal::BadRequest)
    }

    /// The new escrow key is escrowed: it is the pod's.
    pub fn hold_pending_escrow(&mut self) -> Result<(), Refusal> {
        if self.revoked {
            return Err(Refusal::Revoked);
        }
        if self.escrow.is_some() {
            return Err(Refusal::Sealed);
        }
        self.escrow = Some(self.pending_escrow.take().ok_or(Refusal::NoKey)?);
        Ok(())
    }

    /// The escrow key a retrieval opened to (PKCS#8 PEM), P-256 only: it is the pod's.
    pub fn hold_retrieved_escrow(&mut self, pem: &str) -> Result<(), Refusal> {
        if self.revoked {
            return Err(Refusal::Revoked);
        }
        if self.escrow.is_some() || !self.plan.as_ref().is_some_and(|plan| plan.escrow) {
            return Err(Refusal::Sealed);
        }
        self.escrow = Some(EscrowKey::from_pkcs8_pem(pem).map_err(Refusal::BadCertificate)?);
        self.pending_escrow = None;
        Ok(())
    }

    /// A signature over the daemon's own handshake with the custodian's key: a TLS 1.3 CertificateVerify input, and
    /// nothing once revoked. In process only (`own`).
    pub fn sign_own(&self, message: &[u8]) -> Result<Vec<u8>, Refusal> {
        if self.revoked {
            return Err(Refusal::Revoked);
        }
        let held = self.slots.get(&Slot::Custodian).ok_or(Refusal::NoKey)?;
        if held.chain.is_none() {
            return Err(Refusal::NoCertificate);
        }
        if !is_tls13_certificate_verify(message) {
            return Err(Refusal::NotTls13Handshake);
        }
        Ok(held.key.sign(message))
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
    fn nothing_is_signed_before_a_certificate_is_installed() {
        let ca = test_ca();
        let mut custody = Custody::new(&ca.pem).unwrap();
        let message = certificate_verify_input(CLIENT_CONTEXT, &[9; 32]);
        assert_eq!(
            sign(&custody, WORKLOAD, message.clone()),
            Reply::Refused(Refusal::NoKey)
        );
        csr(&mut custody, Slot::Client);
        assert_eq!(
            sign(&custody, WORKLOAD, message),
            Reply::Refused(Refusal::NoCertificate)
        );
    }

    /// In a pod polar-init and the workload are one UID, so one caller holds both roles
    /// (docs/architecture/key-custody.md, step 4, item 1).
    const ONE_UID: Roles = Roles {
        installer: true,
        workload: true,
    };

    // A slot is sealed once a certificate is installed in it: the workload, which runs after polar-init, cannot
    // replace the key or the certificate. The other slot is not sealed.
    #[test]
    fn an_installed_slot_is_sealed_and_the_other_is_not() {
        let ca = test_ca();
        let (mut custody, certificate) = installed(&ca);
        let held = custody.handle_read(ONE_UID, Request::Certificate { slot: Slot::Client });
        let make = |slot| Request::MakeCsr {
            slot,
            identity: IDENTITY.to_string(),
            names: vec![],
        };
        let install = Request::Install {
            slot: Slot::Client,
            chain_pem: vec![certificate.clone()],
        };
        for request in [make(Slot::Client), install] {
            assert_eq!(
                custody.handle(ONE_UID, request, time_now()),
                Reply::Refused(Refusal::Sealed)
            );
        }
        assert_eq!(
            custody.handle_read(ONE_UID, Request::Certificate { slot: Slot::Client }),
            held,
            "the certificate is the one installed"
        );
        let message = certificate_verify_input(CLIENT_CONTEXT, &[9; 32]);
        let Reply::Signature(der) = sign(&custody, WORKLOAD, message.clone()) else {
            panic!("it still signs");
        };
        assert!(
            verifying_key_of(&certificate)
                .verify(&message, &Signature::from_der(&der).unwrap())
                .is_ok(),
            "with the key it was installed for"
        );
        assert!(matches!(
            custody.handle(ONE_UID, make(Slot::Server), time_now()),
            Reply::Csr(_)
        ));
    }

    #[test]
    fn a_refused_certificate_does_not_seal_the_slot() {
        let (ca, other) = (test_ca(), test_ca());
        let mut custody = Custody::new(&ca.pem).unwrap();
        let csr_pem = csr(&mut custody, Slot::Client);
        let install = |chain| Request::Install {
            slot: Slot::Client,
            chain_pem: vec![chain],
        };
        assert!(matches!(
            custody.handle(INSTALLER, install(certify(&other, &csr_pem)), time_now()),
            Reply::Refused(Refusal::BadCertificate(_))
        ));
        assert_eq!(
            custody.handle(INSTALLER, install(certify(&ca, &csr_pem)), time_now()),
            Reply::Installed
        );
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

    /// The serial of `certificate`, as the credentials agent's ledger writes it.
    fn serial_of(certificate: &str) -> String {
        cert_issuer_common::serial::serial_from_pem(certificate).unwrap()
    }

    fn handshake() -> Vec<u8> {
        certificate_verify_input(CLIENT_CONTEXT, &[7; 32])
    }

    #[test]
    fn its_own_serial_revoked_destroys_every_key_and_refuses_everything_after() {
        let ca = test_ca();
        let (mut custody, certificate) = installed(&ca);
        let server_csr = csr(&mut custody, Slot::Server);
        assert_eq!(custody.serial(Slot::Client), Some(serial_of(&certificate)));
        assert_eq!(
            custody.revoke(&["00ff".to_string(), serial_of(&certificate)]),
            vec![Slot::Client]
        );
        assert!(custody.is_revoked());
        assert!(
            custody.slots.is_empty(),
            "every key destroyed, the server slot's too"
        );
        assert_eq!(
            sign(&custody, WORKLOAD, handshake()),
            Reply::Refused(Refusal::Revoked)
        );
        for request in [
            Request::Certificate { slot: Slot::Client },
            Request::MakeCsr {
                slot: Slot::Client,
                identity: IDENTITY.to_string(),
                names: vec![],
            },
            Request::Install {
                slot: Slot::Server,
                chain_pem: vec![certify(&ca, &server_csr)],
            },
        ] {
            assert_eq!(
                custody.handle(INSTALLER, request, time_now()),
                Reply::Refused(Refusal::Revoked)
            );
        }
        assert_eq!(custody.serial(Slot::Client), None);
    }

    #[test]
    fn another_serial_revoked_changes_nothing() {
        let ca = test_ca();
        let (mut custody, _) = installed(&ca);
        assert!(custody.revoke(&["00ff".to_string()]).is_empty());
        assert!(custody.revoke(&[]).is_empty());
        assert!(!custody.is_revoked());
        assert!(matches!(
            sign(&custody, WORKLOAD, handshake()),
            Reply::Signature(_)
        ));
    }

    #[test]
    fn a_server_slot_s_serial_revokes_it() {
        let ca = test_ca();
        let mut custody = Custody::new(&ca.pem).unwrap();
        let server = certify(&ca, &csr(&mut custody, Slot::Server));
        assert_eq!(
            custody.handle(
                INSTALLER,
                Request::Install {
                    slot: Slot::Server,
                    chain_pem: vec![server.clone()]
                },
                time_now()
            ),
            Reply::Installed
        );
        assert_eq!(custody.revoke(&[serial_of(&server)]), vec![Slot::Server]);
        assert!(custody.is_revoked());
    }

    #[test]
    fn a_key_with_no_certificate_has_no_serial_to_revoke() {
        let ca = test_ca();
        let mut custody = Custody::new(&ca.pem).unwrap();
        csr(&mut custody, Slot::Client);
        assert_eq!(custody.serial(Slot::Client), None);
        assert!(custody.revoke(&["00ff".to_string()]).is_empty());
        assert!(!custody.is_revoked());
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
            let slot = prop_oneof![
                Just(Slot::Client),
                Just(Slot::Server),
                Just(Slot::Custodian)
            ];
            prop_oneof![
                (
                    "[a-z.-]{1,30}",
                    any::<bool>(),
                    prop::option::of(prop::collection::vec("[a-z.]{1,12}", 0..2)),
                    any::<bool>()
                )
                    .prop_map(|(identity, client, server, escrow)| {
                        Request::Bootstrap {
                            identity,
                            client,
                            server,
                            escrow,
                        }
                    }),
                Just(Request::Ready),
                prop::collection::vec(any::<u8>(), 0..160)
                    .prop_map(|wrapped| Request::Unwrap { wrapped }),
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

            /// Once a slot is installed, nothing any caller asks changes its key or its certificate.
            #[test]
            fn an_installed_slot_never_changes(requests in prop::collection::vec((request(), any::<bool>(), any::<bool>()), 0..12)) {
                let (mut custody, certificate) = installed(ca());
                let key = custody.slots[&Slot::Client].key.spki_der();
                for (request, installer, workload) in requests {
                    custody.handle(Roles { installer, workload }, request, time_now());
                }
                let held = &custody.slots[&Slot::Client];
                prop_assert_eq!(held.key.spki_der(), key);
                prop_assert_eq!(held.chain.clone(), Some(vec![certificate.clone()]));
            }

            /// For any sequence of revoked sets, the custody signs exactly while no set it was given has held its
            /// own serial, and once one has, never again whatever follows.
            #[test]
            fn it_signs_exactly_until_its_serial_is_revoked(sets in prop::collection::vec((any::<bool>(), prop::collection::vec("[0-9a-f]{2,8}", 0..3)), 0..8)) {
                let (mut custody, certificate) = installed(ca());
                let own = cert_issuer_common::serial::serial_from_pem(&certificate).unwrap();
                let mut seen_own = false;
                for (with_own, others) in sets {
                    let mut set: Vec<String> = others.into_iter().filter(|s| *s != own).collect();
                    if with_own {
                        set.push(own.clone());
                    }
                    custody.revoke(&set);
                    seen_own |= with_own;
                    let signed = matches!(sign(&custody, WORKLOAD, handshake()), Reply::Signature(_));
                    prop_assert_eq!(signed, !seen_own);
                }
            }

            #[test]
            fn no_request_from_any_caller_gets_a_key_out(requests in prop::collection::vec((request(), any::<bool>(), any::<bool>()), 0..12)) {
                let (mut custody, _) = installed(ca());
                for (request, installer, workload) in requests {
                    let reply = custody.handle(Roles { installer, workload }, request, time_now());
                    let bytes = format!("{reply:?}").into_bytes();
                    let encoded = rkyv::to_bytes::<rkyv::rancor::Error>(&reply).unwrap().to_vec();
                    let mut secrets: Vec<Vec<u8>> = custody.slots.values().map(|held| held.key.secret_bytes()).collect();
                    secrets.extend(custody.escrow.iter().chain(custody.pending_escrow.iter()).map(|k| k.secret_bytes()));
                    for secret in secrets {
                        prop_assert!(!encoded.windows(secret.len()).any(|w| w == secret.as_slice()));
                        let hex: String = secret.iter().map(|b| format!("{b:02x}")).collect();
                        prop_assert!(!String::from_utf8_lossy(&bytes).contains(&hex));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod custodian_tests {
    //! The custodian (docs/architecture/key-custody.md, "Part A's bootstrap: the custodian"): its slot, its plan, what it
    //! fills, and the escrow key, which only unwraps.
    use super::tests::{IDENTITY, TestCa, certify, test_ca, time_now};
    use super::*;
    use crate::tls::{CLIENT_CONTEXT, certificate_verify_input};
    use proptest::prelude::*;

    const INSTALLER: Roles = Roles {
        installer: true,
        workload: false,
    };
    const WORKLOAD: Roles = Roles {
        installer: false,
        workload: true,
    };

    fn custodian() -> String {
        cert_issuer_common::custody::custodian_identity(IDENTITY)
    }

    /// What cert-issuer signs for a custodian: its CSR, with the custody names it asked for.
    fn certify_custodian(ca: &TestCa, csr_pem: &str, permissions: &[CustodyPermission]) -> String {
        let mut csr = rcgen::CertificateSigningRequestParams::from_pem(csr_pem).unwrap();
        for p in permissions {
            csr.params
                .subject_alt_names
                .push(rcgen::SanType::URI(p.to_san_string().try_into().unwrap()));
        }
        let now = time_now();
        csr.params.not_before =
            rcgen::date_time_ymd(1970, 1, 1) + std::time::Duration::from_secs((now - 60) as u64);
        csr.params.not_after =
            rcgen::date_time_ymd(1970, 1, 1) + std::time::Duration::from_secs((now + 3600) as u64);
        csr.params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
        csr.signed_by(&ca.issuer).unwrap().pem()
    }

    /// A custody whose custodian slot holds a certificate naming `permissions`.
    fn with_custodian(ca: &TestCa, permissions: &[CustodyPermission]) -> (Custody, String) {
        let mut custody = Custody::new(&ca.pem).unwrap();
        let Reply::Csr(csr) = custody.handle(
            INSTALLER,
            Request::MakeCsr {
                slot: Slot::Custodian,
                identity: custodian(),
                names: vec![],
            },
            time_now(),
        ) else {
            panic!("a CSR");
        };
        let certificate = certify_custodian(ca, &csr, permissions);
        assert_eq!(
            custody.handle(
                INSTALLER,
                Request::Install {
                    slot: Slot::Custodian,
                    chain_pem: vec![certificate.clone()],
                },
                time_now(),
            ),
            Reply::Installed
        );
        (custody, certificate)
    }

    fn plan(client: bool, server: Option<Vec<String>>, escrow: bool) -> Request {
        Request::Bootstrap {
            identity: IDENTITY.to_string(),
            client,
            server,
            escrow,
        }
    }

    fn handshake() -> Vec<u8> {
        certificate_verify_input(CLIENT_CONTEXT, &[3; 32])
    }

    #[test]
    fn the_custodian_slot_holds_a_custodian_s_identity_and_no_other_slot_does() {
        let ca = test_ca();
        let mut custody = Custody::new(&ca.pem).unwrap();
        for (slot, identity) in [
            (Slot::Custodian, IDENTITY.to_string()),
            (Slot::Client, custodian()),
            (Slot::Server, custodian()),
        ] {
            let reply = custody.handle(
                INSTALLER,
                Request::MakeCsr {
                    slot,
                    identity: identity.clone(),
                    names: vec![],
                },
                time_now(),
            );
            assert!(
                matches!(reply, Reply::Refused(Refusal::BadRequest(_))),
                "{slot:?} {identity}: {reply:?}"
            );
        }
    }

    #[test]
    fn the_custodian_s_key_signs_the_daemon_s_own_handshake_and_no_caller_s() {
        let ca = test_ca();
        let (custody, _) = with_custodian(&ca, &[]);
        for roles in [
            INSTALLER,
            WORKLOAD,
            Roles {
                installer: true,
                workload: true,
            },
        ] {
            assert_eq!(
                custody.handle_read(
                    roles,
                    Request::Sign {
                        slot: Slot::Custodian,
                        message: handshake()
                    }
                ),
                Reply::Refused(Refusal::NotPermitted)
            );
        }
        assert!(custody.sign_own(&handshake()).is_ok());
        assert_eq!(
            custody.sign_own(b"anything else"),
            Err(Refusal::NotTls13Handshake)
        );
    }

    #[test]
    fn a_plan_is_for_the_custodian_s_workload_only_what_its_certificate_names_and_once() {
        let ca = test_ca();
        // Before the custodian's certificate: nothing to plan for.
        let mut bare = Custody::new(&ca.pem).unwrap();
        assert_eq!(
            bare.handle(INSTALLER, plan(true, None, false), time_now()),
            Reply::Refused(Refusal::NoCertificate)
        );
        let (mut custody, _) = with_custodian(&ca, &[CustodyPermission::Client]);
        // Another workload's.
        let other = Request::Bootstrap {
            identity: "kube-consumer.polar.serviceaccount.cluster.local".to_string(),
            client: true,
            server: None,
            escrow: false,
        };
        assert!(matches!(
            custody.handle(INSTALLER, other, time_now()),
            Reply::Refused(Refusal::BadRequest(_))
        ));
        // What its certificate does not name.
        for unnamed in [plan(true, Some(vec![]), false), plan(true, None, true)] {
            assert!(matches!(
                custody.handle(INSTALLER, unnamed, time_now()),
                Reply::Refused(Refusal::BadRequest(_))
            ));
        }
        // Not the workload's.
        assert_eq!(
            custody.handle(WORKLOAD, plan(true, None, false), time_now()),
            Reply::Refused(Refusal::NotPermitted)
        );
        assert_eq!(
            custody.handle(INSTALLER, plan(true, None, false), time_now()),
            Reply::Installed
        );
        assert_eq!(
            custody.handle(INSTALLER, plan(true, None, false), time_now()),
            Reply::Refused(Refusal::Sealed)
        );
        // And the workload's slots are the custodian's to fill now, not polar-init's.
        let reply = custody.handle(
            INSTALLER,
            Request::MakeCsr {
                slot: Slot::Client,
                identity: IDENTITY.to_string(),
                names: vec![],
            },
            time_now(),
        );
        assert_eq!(reply, Reply::Refused(Refusal::Sealed));
    }

    #[test]
    fn ready_is_said_once_everything_planned_is_fetched_or_never_if_it_failed() {
        let ca = test_ca();
        let (mut custody, _) = with_custodian(&ca, &[CustodyPermission::Client]);
        assert_eq!(
            custody.handle_read(INSTALLER, Request::Ready),
            Reply::Refused(Refusal::NotReady)
        );
        custody.handle(INSTALLER, plan(true, None, false), time_now());
        assert_eq!(
            custody.handle_read(INSTALLER, Request::Ready),
            Reply::Refused(Refusal::NotReady)
        );
        assert_eq!(
            custody.handle_read(WORKLOAD, Request::Ready),
            Reply::Refused(Refusal::NotPermitted)
        );
        custody.set_ready();
        assert_eq!(custody.handle_read(INSTALLER, Request::Ready), Reply::Ready);
        // A plan with nothing to fetch, a bootstrap service's custodian's, is ready at once.
        let (mut nothing, _) = with_custodian(&ca, &[]);
        assert_eq!(
            nothing.handle(INSTALLER, plan(false, None, false), time_now()),
            Reply::Installed
        );
        assert_eq!(nothing.handle_read(INSTALLER, Request::Ready), Reply::Ready);
        // A failure is said, and stays said.
        let (mut failing, _) = with_custodian(&ca, &[CustodyPermission::Client]);
        failing.handle(INSTALLER, plan(true, None, false), time_now());
        failing.set_failed("refused".to_string());
        failing.set_ready();
        assert_eq!(
            failing.handle_read(INSTALLER, Request::Ready),
            Reply::Refused(Refusal::BootstrapFailed("refused".to_string()))
        );
    }

    #[test]
    fn the_custodian_fills_the_workload_s_slot_for_the_planned_identity_and_the_workload_signs_with_it()
     {
        let ca = test_ca();
        let (mut custody, _) = with_custodian(&ca, &[CustodyPermission::Client]);
        assert_eq!(
            custody.workload_csr(Slot::Client, vec![]),
            Err(Refusal::NotPermitted),
            "no plan yet"
        );
        custody.handle(INSTALLER, plan(true, None, false), time_now());
        assert_eq!(
            custody.workload_csr(Slot::Custodian, vec![]),
            Err(Refusal::NotPermitted)
        );
        let csr = custody.workload_csr(Slot::Client, vec![]).unwrap();
        assert_eq!(
            cert_issuer_common::csr_san::csr_dns_sans(&csr).unwrap(),
            vec![IDENTITY.to_string()]
        );
        custody
            .install_workload(Slot::Client, vec![certify(&ca, &csr)], time_now())
            .unwrap();
        assert!(matches!(
            custody.handle_read(
                WORKLOAD,
                Request::Sign {
                    slot: Slot::Client,
                    message: handshake()
                }
            ),
            Reply::Signature(_)
        ));
        assert_eq!(
            custody.workload_csr(Slot::Client, vec![]),
            Err(Refusal::Sealed)
        );
    }

    #[test]
    fn the_escrow_key_made_here_is_sealed_to_the_pinned_key_and_then_only_unwraps_for_the_workload()
    {
        let ca = test_ca();
        let (mut custody, _) = with_custodian(&ca, &[CustodyPermission::Escrow]);
        assert_eq!(
            custody.escrow_csr(),
            Err(Refusal::NotPermitted),
            "no plan yet"
        );
        custody.handle(INSTALLER, plan(false, None, true), time_now());
        let csr = custody.escrow_csr().unwrap();
        assert_eq!(
            cert_issuer_common::csr_san::csr_dns_sans(&csr).unwrap(),
            vec![IDENTITY.to_string()]
        );
        let credentials_agent = rsa::RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap();
        let sealed = custody
            .sealed_escrow(&credentials_agent.to_public_key())
            .unwrap();
        // Before it is escrowed it unwraps nothing.
        assert_eq!(
            custody.handle_read(
                WORKLOAD,
                Request::Unwrap {
                    wrapped: vec![0; 113]
                }
            ),
            Reply::Refused(Refusal::NoKey)
        );
        custody.hold_pending_escrow().unwrap();
        // The key escrowed is the key held: a secret sealed to its public half unwraps through the daemon.
        let opened = sealed.decrypt(&credentials_agent).unwrap();
        let public =
            crate::escrow::EscrowKey::from_pkcs8_pem(std::str::from_utf8(&opened).unwrap())
                .unwrap()
                .public_key_pem()
                .unwrap();
        let secret = polar_escrow::EscrowedSecret::encrypt(b"a token", &public).unwrap();
        let Reply::Unwrapped(key) = custody.handle_read(
            WORKLOAD,
            Request::Unwrap {
                wrapped: secret.wrapped_key().to_vec(),
            },
        ) else {
            panic!("not unwrapped");
        };
        let plaintext = secret
            .decrypt_with(polar_escrow::Suite::HpkeP256, |_| Ok(key.clone()))
            .unwrap();
        assert_eq!(plaintext.as_slice(), b"a token");
        // Not the installer's, and nothing else is unwrapped.
        assert_eq!(
            custody.handle_read(
                INSTALLER,
                Request::Unwrap {
                    wrapped: secret.wrapped_key().to_vec()
                }
            ),
            Reply::Refused(Refusal::NotPermitted)
        );
        assert!(matches!(
            custody.handle_read(
                WORKLOAD,
                Request::Unwrap {
                    wrapped: vec![1; 113]
                }
            ),
            Reply::Refused(Refusal::BadRequest(_))
        ));
        // Held: no second key.
        assert_eq!(custody.escrow_csr(), Err(Refusal::Sealed));
    }

    #[test]
    fn a_retrieved_escrow_key_is_held_if_it_is_p256_and_revocation_destroys_it() {
        let ca = test_ca();
        let (mut custody, certificate) = with_custodian(&ca, &[CustodyPermission::Escrow]);
        custody.handle(INSTALLER, plan(false, None, true), time_now());
        let rsa = rsa::RsaPrivateKey::new(&mut rand::rng(), 1024).unwrap();
        let rsa_pem =
            rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(&rsa, rsa::pkcs8::LineEnding::LF).unwrap();
        assert!(
            matches!(custody.hold_retrieved_escrow(&rsa_pem), Err(Refusal::BadCertificate(why)) if why.contains("no migration"))
        );
        let key = crate::escrow::EscrowKey::generate().unwrap();
        custody
            .hold_retrieved_escrow(&key.pkcs8_pem().unwrap())
            .unwrap();
        // Held once, never replaced: not by another retrieval, not by a key made here.
        let other = crate::escrow::EscrowKey::generate().unwrap();
        assert_eq!(
            custody.hold_retrieved_escrow(&other.pkcs8_pem().unwrap()),
            Err(Refusal::Sealed)
        );
        assert_eq!(custody.escrow_csr(), Err(Refusal::Sealed));
        assert_eq!(custody.hold_pending_escrow(), Err(Refusal::Sealed));
        assert_eq!(
            custody.escrow.as_ref().map(|k| k.secret_bytes()),
            Some(key.secret_bytes())
        );
        let secret =
            polar_escrow::EscrowedSecret::encrypt(b"x", &key.public_key_pem().unwrap()).unwrap();
        assert!(matches!(
            custody.handle_read(
                WORKLOAD,
                Request::Unwrap {
                    wrapped: secret.wrapped_key().to_vec()
                }
            ),
            Reply::Unwrapped(_)
        ));
        // The custodian's own certificate revoked: every key goes, the escrow key with them.
        let serial = cert_issuer_common::serial::serial_from_pem(&certificate).unwrap();
        assert_eq!(custody.revoke(&[serial]), vec![Slot::Custodian]);
        assert_eq!(
            custody.handle_read(
                WORKLOAD,
                Request::Unwrap {
                    wrapped: secret.wrapped_key().to_vec()
                }
            ),
            Reply::Refused(Refusal::Revoked)
        );
        assert!(custody.escrow.is_none() && custody.pending_escrow.is_none());
        assert_eq!(custody.sign_own(&handshake()), Err(Refusal::Revoked));
    }

    proptest! {
        /// Whatever any caller asks, in any order, the custodian's key never signs for it, and the escrow key, once
        /// held, is never replaced.
        #[test]
        fn no_caller_signs_with_the_custodian_s_key_or_replaces_the_escrow_key(
            requests in prop::collection::vec((
                prop_oneof![
                    prop::collection::vec(any::<u8>(), 0..120).prop_map(|message| Request::Sign { slot: Slot::Custodian, message }),
                    Just(Request::Sign { slot: Slot::Custodian, message: certificate_verify_input(CLIENT_CONTEXT, &[3; 32]) }),
                    "[a-z.-]{1,30}".prop_map(|identity| Request::MakeCsr { slot: Slot::Custodian, identity, names: vec![] }),
                    Just(Request::Bootstrap { identity: IDENTITY.to_string(), client: false, server: None, escrow: true }),
                    prop::collection::vec(any::<u8>(), 0..120).prop_map(|wrapped| Request::Unwrap { wrapped }),
                ],
                any::<bool>(),
                any::<bool>(),
            ), 0..10),
        ) {
            let ca = test_ca();
            let (mut custody, _) = with_custodian(&ca, &[CustodyPermission::Escrow]);
            custody.handle(INSTALLER, plan(false, None, true), time_now());
            let key = crate::escrow::EscrowKey::generate().unwrap();
            custody.hold_retrieved_escrow(&key.pkcs8_pem().unwrap()).unwrap();
            let held = key.secret_bytes();
            for (request, installer, workload) in requests {
                let reply = custody.handle(Roles { installer, workload }, request, time_now());
                prop_assert!(!matches!(reply, Reply::Signature(_)), "{reply:?}");
                prop_assert_eq!(custody.escrow.as_ref().map(|k| k.secret_bytes()), Some(held.clone()));
            }
        }
    }
}
