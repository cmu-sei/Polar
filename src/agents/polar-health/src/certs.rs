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

//! Reading the agent's own certificates.
//!
//! Every function takes the current time as a parameter instead of asking the
//! clock, so a certificate's remaining life can be tested at any moment without
//! waiting for it.

use std::fs;
use x509_parser::prelude::*;

/// Seconds of validity left on the PEM certificate at `path`, as of `now`
/// (Unix seconds). Negative once the certificate has expired.
pub(crate) fn ttl_secs(path: &str, now: i64) -> Result<i64, String> {
    let data = fs::read(path).map_err(|e| format!("failed to read cert '{path}': {e}"))?;

    let (_, pem) =
        parse_x509_pem(&data).map_err(|e| format!("failed to parse PEM '{path}': {e}"))?;

    let cert = pem
        .parse_x509()
        .map_err(|e| format!("failed to parse x509 in '{path}': {e}"))?;

    let not_after = cert.validity().not_after.timestamp();
    not_after
        .checked_sub(now)
        .ok_or_else(|| format!("certificate timestamp arithmetic overflow for '{path}'"))
}

/// Checks every certificate against the hard expiry threshold.
/// Returns `Err` describing the first one that is missing, unparseable, or
/// within `expiry_threshold_secs` of expiring.
pub(crate) fn check_hard(
    paths: &[String],
    expiry_threshold_secs: i64,
    now: i64,
) -> Result<(), String> {
    if paths.is_empty() {
        tracing::warn!("healthcheck: no cert paths configured, skipping cert check");
        return Ok(());
    }

    for path in paths {
        let secs_remaining = ttl_secs(path, now)?;

        if secs_remaining < expiry_threshold_secs {
            return Err(format!(
                "cert '{path}' expires in {secs_remaining}s (hard threshold: {expiry_threshold_secs}s)"
            ));
        }

        tracing::debug!("cert '{path}' valid for {secs_remaining}s");
    }

    Ok(())
}

/// The smallest remaining life across all certificates that can be read, or
/// `None` if there are none or none are readable.
pub(crate) fn min_ttl(paths: &[String], now: i64) -> Option<i64> {
    paths
        .iter()
        .filter_map(|path| ttl_secs(path, now).ok())
        .reduce(i64::min)
}

/// What the certificates look like at one moment, gathered once per tick.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CertReading {
    /// `Err` when a certificate is past the hard threshold, or unreadable.
    pub hard: Result<(), String>,
    pub min_ttl: Option<i64>,
    /// What the custody daemon holds, when the keys are its (docs/architecture/key-custody.md, step 4).
    pub custody: crate::custody::Custody,
}

pub(crate) fn read(paths: &[String], expiry_threshold_secs: i64, now: i64) -> CertReading {
    CertReading {
        hard: check_hard(paths, expiry_threshold_secs, now),
        min_ttl: min_ttl(paths, now),
        custody: crate::custody::Custody::NotUsed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{cert_file, now_secs};

    #[test]
    fn check_hard_without_paths_is_ok() {
        assert!(check_hard(&[], 60, now_secs()).is_ok());
    }

    #[test]
    fn check_hard_accepts_a_long_lived_cert() {
        let dir = tempfile::tempdir().unwrap();
        let paths = vec![cert_file(&dir, "a.pem", 3600)];
        assert!(check_hard(&paths, 60, now_secs()).is_ok());
    }

    #[test]
    fn check_hard_rejects_a_cert_inside_the_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let paths = vec![cert_file(&dir, "a.pem", 30)];
        let err = check_hard(&paths, 60, now_secs()).unwrap_err();
        assert!(err.contains("hard threshold"), "{err}");
    }

    #[test]
    fn check_hard_rejects_an_expired_cert() {
        let dir = tempfile::tempdir().unwrap();
        let paths = vec![cert_file(&dir, "a.pem", -10)];
        assert!(check_hard(&paths, 60, now_secs()).is_err());
    }

    #[test]
    fn check_hard_rejects_a_missing_file() {
        let paths = vec!["/nonexistent/polar-health-test.pem".to_string()];
        let err = check_hard(&paths, 60, now_secs()).unwrap_err();
        assert!(err.contains("failed to read cert"), "{err}");
    }

    #[test]
    fn check_hard_rejects_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("junk.pem");
        std::fs::write(&path, "not a pem").unwrap();
        let paths = vec![path.to_string_lossy().into_owned()];
        let err = check_hard(&paths, 60, now_secs()).unwrap_err();
        assert!(err.contains("failed to parse PEM"), "{err}");
    }

    #[test]
    fn check_hard_fails_when_any_one_cert_is_short() {
        let dir = tempfile::tempdir().unwrap();
        let paths = vec![
            cert_file(&dir, "long.pem", 3600),
            cert_file(&dir, "short.pem", 30),
        ];
        assert!(check_hard(&paths, 60, now_secs()).is_err());
    }

    #[test]
    fn min_ttl_is_none_without_paths() {
        assert_eq!(min_ttl(&[], now_secs()), None);
    }

    #[test]
    fn min_ttl_is_the_smallest_remaining() {
        let dir = tempfile::tempdir().unwrap();
        let paths = vec![
            cert_file(&dir, "long.pem", 3600),
            cert_file(&dir, "short.pem", 600),
        ];
        let ttl = min_ttl(&paths, now_secs()).expect("ttl");
        assert!((595..=600).contains(&ttl), "ttl was {ttl}");
    }

    #[test]
    fn min_ttl_skips_unreadable_certs() {
        let dir = tempfile::tempdir().unwrap();
        let paths = vec![
            "/nonexistent/polar-health-test.pem".to_string(),
            cert_file(&dir, "ok.pem", 600),
        ];
        assert!(min_ttl(&paths, now_secs()).is_some());
    }

    #[test]
    fn min_ttl_is_none_when_nothing_is_readable() {
        let paths = vec!["/nonexistent/polar-health-test.pem".to_string()];
        assert_eq!(min_ttl(&paths, now_secs()), None);
    }

    /// The point of passing the time in: the same certificate, judged at two
    /// different moments, without waiting.
    #[test]
    fn the_same_cert_is_judged_at_the_moment_given() {
        let dir = tempfile::tempdir().unwrap();
        let paths = vec![cert_file(&dir, "a.pem", 600)];
        let now = now_secs();
        assert!(check_hard(&paths, 60, now).is_ok());
        assert!(check_hard(&paths, 60, now + 560).is_err());
        let ttl = ttl_secs(&paths[0], now + 500).unwrap();
        assert!((95..=100).contains(&ttl), "ttl was {ttl}");
    }

    #[test]
    fn read_gathers_both_views() {
        let dir = tempfile::tempdir().unwrap();
        let paths = vec![cert_file(&dir, "a.pem", 600)];
        let reading = read(&paths, 60, now_secs());
        assert!(reading.hard.is_ok());
        assert!(reading.min_ttl.is_some());
    }
}
