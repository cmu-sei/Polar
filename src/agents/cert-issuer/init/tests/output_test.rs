//! Tests for writing the cert/key/CA bundle to the shared volume.

use cert_issuer_init::output::{write_bundle, OutputBundle, OutputError};
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
fn writes_three_files_with_expected_names() {
    let dir = TempDir::new().unwrap();
    write_bundle(dir.path(), &bundle()).expect("write should succeed");

    assert_eq!(
        std::fs::read_to_string(dir.path().join("cert.pem")).unwrap(),
        FAKE_CERT,
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("key.pem")).unwrap(),
        FAKE_KEY,
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("ca.pem")).unwrap(),
        FAKE_CA,
    );
}

#[test]
fn key_file_has_restricted_permissions() {
    // The key is the secret. Mode 0640 means owner read/write,
    // group read, others nothing. The shared volume's group should
    // be set up so the workload container's user can read.
    let dir = TempDir::new().unwrap();
    write_bundle(dir.path(), &bundle()).expect("write should succeed");

    let metadata = std::fs::metadata(dir.path().join("key.pem")).unwrap();
    let mode = metadata.permissions().mode() & 0o777;
    assert_eq!(mode, 0o640, "key.pem must be mode 0640, got {:o}", mode);
}

#[test]
fn missing_directory_returns_dir_missing() {
    let bogus = std::path::PathBuf::from("/nonexistent/path/that/does/not/exist");
    let err = write_bundle(&bogus, &bundle()).expect_err("missing dir must error");
    assert!(matches!(err, OutputError::DirMissing(_)));
}

#[test]
fn writes_are_atomic() {
    // If a write fails partway through, no file should be partially
    // written. The implementation writes to .tmp files and renames.
    // This test is structural — we can't easily simulate a partial
    // write — but we assert that no .tmp files are left behind on
    // success. If the renames don't happen, the .tmp files stick
    // around and this test catches it.
    let dir = TempDir::new().unwrap();
    write_bundle(dir.path(), &bundle()).expect("write should succeed");

    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let tmp_files: Vec<_> = entries.iter().filter(|n| n.ends_with(".tmp")).collect();
    assert!(
        tmp_files.is_empty(),
        ".tmp files left behind: {tmp_files:?}",
    );
}

#[test]
fn writing_overwrites_existing_files() {
    // Restart-driven renewal means the init container will run
    // multiple times against the same shared volume. Existing files
    // must be replaced, not appended.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("cert.pem"), "old contents").unwrap();
    std::fs::write(dir.path().join("key.pem"), "old contents").unwrap();
    std::fs::write(dir.path().join("ca.pem"), "old contents").unwrap();

    write_bundle(dir.path(), &bundle()).expect("write should succeed");

    assert_eq!(
        std::fs::read_to_string(dir.path().join("cert.pem")).unwrap(),
        FAKE_CERT,
        "cert.pem must be overwritten, not appended",
    );
}
