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

// tests/output_tests.rs
//! Tests for atomic cert bundle writing to the shared emptyDir volume.

use cert_client::output::{OutputBundle, OutputError, write_bundle};
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

const FAKE_CERT: &str = "-----BEGIN CERTIFICATE-----\nfake-cert\n-----END CERTIFICATE-----\n";
const FAKE_KEY: &str = "-----BEGIN PRIVATE KEY-----\nfake-key\n-----END PRIVATE KEY-----\n";
const FAKE_CA: &str = "-----BEGIN CERTIFICATE-----\nfake-ca\n-----END CERTIFICATE-----\n";

fn bundle() -> OutputBundle<'static> {
    OutputBundle {
        cert_pem: FAKE_CERT,
        key_pem: FAKE_KEY,
        ca_pem: FAKE_CA,
    }
}

#[test]
fn writes_three_files_with_correct_names_and_contents() {
    let dir = TempDir::new().unwrap();
    write_bundle(dir.path(), &bundle()).expect("write must succeed");

    assert_eq!(
        std::fs::read_to_string(dir.path().join("cert.pem")).unwrap(),
        FAKE_CERT
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("key.pem")).unwrap(),
        FAKE_KEY
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("ca.pem")).unwrap(),
        FAKE_CA
    );
}

#[test]
fn key_file_mode_is_0640() {
    // 0640: owner rw, group r, others nothing. The shared emptyDir
    // volume's group must be configured to match the workload
    // container's GID — that's a pod spec concern, not ours. Our
    // job is to set the mode correctly so the workload container
    // can read it without it being world-readable.
    let dir = TempDir::new().unwrap();
    write_bundle(dir.path(), &bundle()).expect("write must succeed");

    let mode = std::fs::metadata(dir.path().join("key.pem"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o640, "key.pem must be 0640, got {:o}", mode);
}

#[test]
fn cert_and_ca_files_are_world_readable() {
    // cert.pem and ca.pem are not secret — they're the public half
    // of the TLS identity. World-readable is fine and avoids
    // permission headaches if the workload container runs as a
    // different UID than the init container.
    let dir = TempDir::new().unwrap();
    write_bundle(dir.path(), &bundle()).expect("write must succeed");

    for filename in ["cert.pem", "ca.pem"] {
        let mode = std::fs::metadata(dir.path().join(filename))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o644, "{filename} must be 0644, got {:o}", mode);
    }
}

#[test]
fn no_tmp_files_left_on_success() {
    // write_bundle writes to .tmp files and renames atomically.
    // A successful write must leave no .tmp artifacts.
    let dir = TempDir::new().unwrap();
    write_bundle(dir.path(), &bundle()).expect("write must succeed");

    let tmp_files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();

    assert!(
        tmp_files.is_empty(),
        ".tmp files left behind: {tmp_files:?}"
    );
}

#[test]
fn overwrites_existing_files_on_restart() {
    // Restart-driven renewal runs the init container again against
    // the same emptyDir. Existing files must be replaced cleanly.
    let dir = TempDir::new().unwrap();
    for name in ["cert.pem", "key.pem", "ca.pem"] {
        std::fs::write(dir.path().join(name), "stale contents from previous run").unwrap();
    }

    write_bundle(dir.path(), &bundle()).expect("write must succeed");

    assert_eq!(
        std::fs::read_to_string(dir.path().join("cert.pem")).unwrap(),
        FAKE_CERT
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("key.pem")).unwrap(),
        FAKE_KEY
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("ca.pem")).unwrap(),
        FAKE_CA
    );
}

#[test]
fn missing_directory_returns_dir_missing_error() {
    let bogus = std::path::PathBuf::from("/nonexistent/path");
    let err = write_bundle(&bogus, &bundle()).expect_err("missing dir must error");
    assert!(
        matches!(err, OutputError::DirMissing(_)),
        "expected DirMissing, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// existing_bundle: a pod's own bundle used again when its init container runs
// again without the pod being replaced
// ---------------------------------------------------------------------------

mod existing {
    use cert_client::output::{ExistingBundle, OutputBundle, Usage, existing_bundle, write_bundle};
    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
        PKCS_ECDSA_P256_SHA256,
    };
    use std::time::{Duration, SystemTime};
    use tempfile::TempDir;

    const IDENTITY: &str = "graph.polar-graph.serviceaccount.cluster.local";

    struct Ca {
        params: CertificateParams,
        key: KeyPair,
        pem: String,
    }

    fn ca() -> Ca {
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let pem = params.self_signed(&key).unwrap().pem();
        Ca { params, key, pem }
    }

    /// A leaf signed by `ca` for `names`, with `usages`, valid until `days` from now (negative: already expired).
    fn leaf(
        ca: &Ca,
        names: &[&str],
        usages: Vec<ExtendedKeyUsagePurpose>,
        days: i64,
    ) -> (String, String) {
        let mut params =
            CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
                .unwrap();
        params.extended_key_usages = usages;
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::days(400);
        params.not_after = now + time::Duration::days(days);
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let issuer = Issuer::from_params(&ca.params, &ca.key);
        let cert = params.signed_by(&key, &issuer).unwrap();
        (cert.pem(), key.serialize_pem())
    }

    fn server_and_client() -> Vec<ExtendedKeyUsagePurpose> {
        vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ]
    }

    fn written(cert_pem: &str, key_pem: &str, ca_pem: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        write_bundle(
            dir.path(),
            &OutputBundle {
                cert_pem,
                key_pem,
                ca_pem,
            },
        )
        .unwrap();
        dir
    }

    fn judge(dir: &TempDir, names: &[&str], usage: Usage) -> ExistingBundle {
        let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        existing_bundle(dir.path(), IDENTITY, &names, usage, SystemTime::now())
    }

    fn unusable_because(result: ExistingBundle, words: &str) {
        match result {
            ExistingBundle::Unusable(why) => {
                assert!(why.contains(words), "expected '{words}' in: {why}")
            }
            other => panic!("expected Unusable ({words}), got {other:?}"),
        }
    }

    #[test]
    fn an_empty_directory_has_nothing_to_use() {
        let dir = TempDir::new().unwrap();
        assert_eq!(judge(&dir, &[], Usage::Server), ExistingBundle::Absent);
    }

    #[test]
    fn the_pods_own_valid_bundle_is_used_again() {
        let ca = ca();
        let (cert, key) = leaf(
            &ca,
            &[IDENTITY, "neo4j", "polar-db-svc"],
            server_and_client(),
            365,
        );
        let dir = written(&cert, &key, &ca.pem);
        assert!(matches!(
            judge(&dir, &["neo4j", "polar-db-svc"], Usage::Server),
            ExistingBundle::Reusable { .. }
        ));
        assert!(matches!(
            judge(&dir, &[], Usage::Client),
            ExistingBundle::Reusable { .. }
        ));
    }

    #[test]
    fn an_expired_certificate_is_not_used() {
        let ca = ca();
        let (cert, key) = leaf(&ca, &[IDENTITY], server_and_client(), -1);
        unusable_because(
            judge(&written(&cert, &key, &ca.pem), &[], Usage::Server),
            "not valid now",
        );
    }

    #[test]
    fn a_certificate_judged_after_its_expiry_is_not_used() {
        let ca = ca();
        let (cert, key) = leaf(&ca, &[IDENTITY], server_and_client(), 1);
        let dir = written(&cert, &key, &ca.pem);
        let later = SystemTime::now() + Duration::from_secs(2 * 24 * 3600);
        unusable_because(
            existing_bundle(dir.path(), IDENTITY, &[], Usage::Server, later),
            "not valid now",
        );
    }

    #[test]
    fn a_certificate_from_another_ca_is_not_used() {
        let (theirs, ours) = (ca(), ca());
        let (cert, key) = leaf(&theirs, &[IDENTITY], server_and_client(), 365);
        unusable_because(
            judge(&written(&cert, &key, &ours.pem), &[], Usage::Server),
            "not signed by ca.pem",
        );
    }

    #[test]
    fn a_key_that_is_not_the_certificates_is_not_used() {
        let ca = ca();
        let (cert, _) = leaf(&ca, &[IDENTITY], server_and_client(), 365);
        let (_, other_key) = leaf(&ca, &[IDENTITY], server_and_client(), 365);
        unusable_because(
            judge(&written(&cert, &other_key, &ca.pem), &[], Usage::Server),
            "not the certificate's key",
        );
    }

    #[test]
    fn a_certificate_for_another_identity_or_without_a_requested_name_is_not_used() {
        let ca = ca();
        let (cert, key) = leaf(
            &ca,
            &["someone-else.polar.serviceaccount.cluster.local"],
            server_and_client(),
            365,
        );
        unusable_because(
            judge(&written(&cert, &key, &ca.pem), &[], Usage::Server),
            IDENTITY,
        );

        let (cert, key) = leaf(&ca, &[IDENTITY, "neo4j"], server_and_client(), 365);
        unusable_because(
            judge(
                &written(&cert, &key, &ca.pem),
                &["neo4j", "polar-db-svc"],
                Usage::Server,
            ),
            "polar-db-svc",
        );
    }

    #[test]
    fn a_certificate_without_the_requested_usage_is_not_used() {
        let ca = ca();
        let (cert, key) = leaf(
            &ca,
            &[IDENTITY],
            vec![ExtendedKeyUsagePurpose::ClientAuth],
            365,
        );
        unusable_because(
            judge(&written(&cert, &key, &ca.pem), &[], Usage::Server),
            "not for Server use",
        );
    }

    #[test]
    fn files_that_are_not_a_bundle_are_not_used() {
        let dir = written(
            "-----BEGIN CERTIFICATE-----\nfake-cert\n-----END CERTIFICATE-----\n",
            "-----BEGIN PRIVATE KEY-----\nfake-key\n-----END PRIVATE KEY-----\n",
            "-----BEGIN CERTIFICATE-----\nfake-ca\n-----END CERTIFICATE-----\n",
        );
        assert!(matches!(
            judge(&dir, &[], Usage::Server),
            ExistingBundle::Unusable(_)
        ));

        let half = TempDir::new().unwrap();
        std::fs::write(half.path().join("cert.pem"), "x").unwrap();
        unusable_because(judge(&half, &[], Usage::Server), "key.pem cannot be read");
    }
}
