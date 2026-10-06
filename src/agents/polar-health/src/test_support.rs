//! Helpers shared by the crate's unit tests.

use ::time::OffsetDateTime;
use std::fs;

pub(crate) fn now_secs() -> i64 {
    crate::HealthState::now() as i64
}

/// PEM for a self-signed certificate valid from an hour ago until `ttl_secs`
/// from now (negative = already expired).
pub(crate) fn cert_pem(ttl_secs: i64) -> String {
    let mut params =
        rcgen::CertificateParams::new(vec!["test.local".to_string()]).expect("cert params");
    params.not_before = OffsetDateTime::now_utc() - ::time::Duration::hours(1);
    params.not_after = OffsetDateTime::now_utc() + ::time::Duration::seconds(ttl_secs);
    let key = rcgen::KeyPair::generate().expect("key");
    params.self_signed(&key).expect("self sign").pem()
}

/// Writes such a certificate into `dir` and returns its path.
pub(crate) fn cert_file(dir: &tempfile::TempDir, name: &str, ttl_secs: i64) -> String {
    let path = dir.path().join(name);
    fs::write(&path, cert_pem(ttl_secs)).expect("write cert");
    path.to_string_lossy().into_owned()
}
