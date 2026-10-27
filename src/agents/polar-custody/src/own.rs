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

//! The daemon's own handshake (docs/architecture/key-custody.md, step 5): its watcher connects to Cassini as the pod,
//! presenting the client slot's certificate, and signs in this process with the slot's key, through the same rule
//! as every signature it makes (a TLS 1.3 CertificateVerify input, and nothing once revoked). Nothing here reaches the
//! socket, and nothing gives the key out.

use crate::custody::{Custody, Roles};
use crate::protocol::{Reply, Request, Slot};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;
use rustls::sign::{CertifiedKey, Signer, SigningKey};
use rustls::{SignatureAlgorithm, SignatureScheme};
use std::sync::{Arc, RwLock};

/// What the daemon's own signatures are asked as: the workload's role, so they pass every check a workload's does.
const OWN: Roles = Roles {
    installer: false,
    workload: true,
};

/// A rustls signing key over a slot of this process's custody: ECDSA P-256 SHA-256, TLS 1.3 handshakes only.
#[derive(Debug, Clone)]
pub struct OwnSigningKey {
    custody: Arc<RwLock<Custody>>,
    slot: Slot,
}

impl SigningKey for OwnSigningKey {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
        offered
            .contains(&SignatureScheme::ECDSA_NISTP256_SHA256)
            .then(|| Box::new(self.clone()) as Box<dyn Signer>)
    }

    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::ECDSA
    }
}

impl Signer for OwnSigningKey {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rustls::Error> {
        let custody = self
            .custody
            .read()
            .map_err(|_| rustls::Error::General("the custody's lock is poisoned".to_string()))?;
        match custody.handle_read(
            OWN,
            Request::Sign {
                slot: self.slot,
                message: message.to_vec(),
            },
        ) {
            Reply::Signature(signature) => Ok(signature),
            other => Err(rustls::Error::General(format!(
                "the custody did not sign its own handshake: {other:?}"
            ))),
        }
    }

    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::ECDSA_NISTP256_SHA256
    }
}

/// Hands rustls the client slot's certificate and its in-process key whenever a server asks for a client certificate.
#[derive(Debug)]
struct OwnClientCert(Arc<CertifiedKey>);

impl rustls::client::ResolvesClientCert for OwnClientCert {
    fn resolve(&self, _: &[&[u8]], _: &[SignatureScheme]) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }

    fn has_certs(&self) -> bool {
        true
    }
}

/// A client configuration for the daemon's own connection: the client slot's certificate, signed for in process,
/// TLS 1.3 alone, the server verified under `ca_pem`. An error while the slot holds no certificate.
pub fn own_client_config(
    custody: Arc<RwLock<Custody>>,
    ca_pem: &str,
) -> Result<rustls::ClientConfig, String> {
    let chain = custody
        .read()
        .map_err(|_| "the custody's lock is poisoned".to_string())?
        .chain(Slot::Client)
        .ok_or("the client slot holds no certificate yet")?
        .iter()
        .map(|pem| {
            CertificateDer::from_pem_slice(pem.as_bytes())
                .map_err(|e| format!("the slot's certificate: {e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(
            CertificateDer::from_pem_slice(ca_pem.as_bytes())
                .map_err(|e| format!("the pinned CA: {e}"))?,
        )
        .map_err(|e| format!("the pinned CA: {e}"))?;
    let verifier = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| format!("the server verifier: {e}"))?;
    let key = OwnSigningKey {
        custody,
        slot: Slot::Client,
    };
    Ok(
        rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_webpki_verifier(verifier)
            .with_client_cert_resolver(Arc::new(OwnClientCert(Arc::new(CertifiedKey::new(
                chain,
                Arc::new(key),
            ))))),
    )
}
