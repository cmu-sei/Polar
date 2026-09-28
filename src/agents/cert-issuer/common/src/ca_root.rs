//! Generating a CA root keypair and its self-signed certificate.
//!
//! Lives in cert-issuer-common, not in the server, so the admin tool's
//! `bootstrap-ca` can generate a CA without depending on the whole
//! server crate. Moved verbatim from cert-issuer/server's ca.rs; the
//! server re-exports it, so existing callers are unchanged.

use rcgen::KeyPair;
use time::OffsetDateTime;

/// Generate a fresh CA root keypair and self-signed certificate.
///
/// Used at bootstrap time (run once per environment) to produce the
/// CA materials the cert issuer will load from disk on subsequent
/// startups. The output is two PEM strings: the CA cert and the CA
/// key.
///
/// Validity defaults to 10 years. CA roots are long-lived; rotation
/// is a separate operational procedure (see module docs).
///
/// This is intentionally exposed as a library function rather than
/// a separate binary so it can be invoked from a small `cargo run
/// --bin generate-ca` shim or from a test.
pub fn bootstrap_ca(common_name: &str) -> Result<(String, String), String> {
    let mut params: rcgen::CertificateParams = Default::default();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, common_name);
    params.not_before = OffsetDateTime::now_utc();
    params.not_after = OffsetDateTime::now_utc() + time::Duration::days(365 * 10);

    let key_pair = KeyPair::generate().map_err(|e| format!("ca keygen: {e}"))?;
    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| format!("ca self-sign: {e}"))?;

    Ok((cert.pem(), key_pair.serialize_pem()))
}
