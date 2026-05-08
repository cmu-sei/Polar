// tests/output_tests.rs
//! Tests for atomic cert bundle writing to the shared emptyDir volume.

use cert_issuer_init::output::{OutputBundle, OutputError, write_bundle};
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
