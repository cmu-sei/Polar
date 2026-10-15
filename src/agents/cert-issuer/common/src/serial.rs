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

//! The one place a certificate's serial is turned into a string.
//! The ledger, the admin CLI and cert-issuer must all agree on it.

use x509_parser::certificate::X509Certificate;

/// Contiguous lowercase hex of the raw big-endian serial bytes, the
/// format `openssl x509 -text` conventionally displays.
pub fn serial_from_cert(cert: &X509Certificate<'_>) -> String {
    cert.tbs_certificate
        .raw_serial()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn serial_from_pem(pem: &str) -> Option<String> {
    let (_, pem_block) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).ok()?;
    let (_, cert) = x509_parser::parse_x509_certificate(&pem_block.contents).ok()?;
    Some(serial_from_cert(&cert))
}
