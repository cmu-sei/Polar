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
