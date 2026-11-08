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
