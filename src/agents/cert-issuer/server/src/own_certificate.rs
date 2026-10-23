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

//! cert-issuer's own client certificate (docs/architecture/cert-issuer-isolation.md, "1b in detail"). cert-issuer is a
//! bootstrap service too, so it is signed its certificate as they are: on its own service account token, through its
//! own handler, so its floor entry decides its names (its groups and topics) and an enforced floor with no entry for it
//! refuses it. Nothing about it goes around the floor.

use crate::handler::{Handler, HandlerBody, ValidateHandlerBody};
use cert_issuer_common::keypair::{KeyAlgorithm, generate_csr};
use cert_issuer_common::{CertType, IssueRequest, ValidateRequest, ValidateResponse};
use std::path::Path;

/// A certificate cert-issuer was signed for itself, with its key.
#[derive(Debug, Clone)]
pub struct OwnCertificate {
    pub certificate_pem: String,
    pub private_key_pem: String,
    pub ca_chain_pem: String,
}

/// cert-issuer's own client certificate, signed on `token` by `handler`.
pub async fn issue_own(handler: &Handler, token: &str) -> Result<OwnCertificate, String> {
    // Its identity as its own token gives it, in the DNS form its certificate carries.
    let validated = handler
        .handle_validate(
            None,
            token,
            ValidateRequest {
                include_normalized_identity: true,
            },
        )
        .await;
    let identity = match validated.body {
        ValidateHandlerBody::Success(ValidateResponse {
            normalized_identity: Some(identity),
            ..
        }) => identity,
        ValidateHandlerBody::Success(_) => {
            return Err("its own token was validated, but no identity came back".to_string());
        }
        ValidateHandlerBody::Error(e) => {
            return Err(format!(
                "its own token was refused: {} ({:?})",
                e.detail, e.outcome
            ));
        }
    };
    let csr = generate_csr(&identity, &KeyAlgorithm::Ed25519, &[])
        .map_err(|e| format!("its own key and request could not be made: {e}"))?;
    let issued = handler
        .handle(
            None,
            token,
            IssueRequest {
                csr_pem: csr.csr_pem,
                cert_type: CertType::Client,
                extra_sans: Vec::new(),
                subject_dn_defaults: None,
            },
        )
        .await;
    match issued.body {
        HandlerBody::Success(response) => Ok(OwnCertificate {
            certificate_pem: response.certificate_pem,
            private_key_pem: csr.private_key_pem,
            ca_chain_pem: response.ca_chain_pem,
        }),
        HandlerBody::Error(e) => Err(format!(
            "it was refused its own certificate: {} ({:?})",
            e.detail, e.outcome
        )),
    }
}

impl OwnCertificate {
    /// Writes the certificate and its key where the Cassini client reads them, the key readable by its owner alone.
    pub fn write(&self, certificate: &Path, key: &Path) -> Result<(), String> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        for path in [certificate, key] {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot make {}: {e}", parent.display()))?;
            }
        }
        std::fs::write(certificate, &self.certificate_pem)
            .map_err(|e| format!("cannot write {}: {e}", certificate.display()))?;
        // Made private before the key is in it, never loosened after.
        let _ = std::fs::remove_file(key);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(key)
            .and_then(|mut file| file.write_all(self.private_key_pem.as_bytes()))
            .map_err(|e| format!("cannot write {}: {e}", key.display()))
    }
}
