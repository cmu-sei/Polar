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

//! What is read from a certificate, in one place.

use crate::document::Algorithm;
use x509_parser::certificate::X509Certificate;

const OID_ED25519: &str = "1.3.101.112";
const OID_EC_PUBLIC_KEY: &str = "1.2.840.10045.2.1";
const OID_P256: &str = "1.2.840.10045.3.1.7";

/// The DER of the first certificate in a PEM string.
pub(crate) fn der_from_pem(pem: &str) -> Result<Vec<u8>, String> {
    x509_parser::pem::parse_x509_pem(pem.as_bytes())
        .map(|(_, block)| block.contents)
        .map_err(|e| e.to_string())
}

pub(crate) fn parse(der: &[u8]) -> Result<X509Certificate<'_>, String> {
    x509_parser::parse_x509_certificate(der)
        .map(|(_, cert)| cert)
        .map_err(|e| e.to_string())
}

/// The signature algorithm the certificate's key supports, and the key's bytes, if it is one this
/// system signs with: Ed25519 or ECDSA on P-256.
pub(crate) fn key_of(cert: &X509Certificate<'_>) -> Option<(Algorithm, Vec<u8>)> {
    let spki = cert.public_key();
    let oid = spki.algorithm.algorithm.to_id_string();
    let bytes = spki.subject_public_key.data.to_vec();
    if oid == OID_ED25519 {
        return Some((Algorithm::Ed25519, bytes));
    }
    if oid == OID_EC_PUBLIC_KEY {
        let curve = spki
            .algorithm
            .parameters
            .as_ref()
            .and_then(|p| p.as_oid().ok())
            .map(|o| o.to_id_string());
        if curve.as_deref() == Some(OID_P256) {
            return Some((Algorithm::EcdsaP256Sha256, bytes));
        }
    }
    None
}
