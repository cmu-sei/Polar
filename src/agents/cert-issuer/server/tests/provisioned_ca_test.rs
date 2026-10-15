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

//! Tests for `load_provisioned_ca` -- the loader for a CA an operator
//! generated ahead of time. What matters: it never generates anything,
//! it refuses a key other users could read or write, and it still
//! catches a cert/key pair that don't belong together.

#![cfg(unix)]

use cert_issuer::ca::{CaMaterialsError, load_provisioned_ca};
use cert_issuer_common::ca_root::bootstrap_ca;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn write(path: &Path, contents: &str, mode: u32) {
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn paths(dir: &Path) -> (String, String) {
    (
        dir.join("ca.crt").to_str().unwrap().to_string(),
        dir.join("ca.key").to_str().unwrap().to_string(),
    )
}

#[test]
fn missing_materials_are_an_error_and_nothing_is_generated() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = paths(dir.path());

    let err = load_provisioned_ca(&cert, &key).expect_err("must not succeed");

    assert!(matches!(err, CaMaterialsError::NotProvisioned { .. }));
    assert!(!Path::new(&cert).exists(), "must not have written a cert");
    assert!(!Path::new(&key).exists(), "must not have written a key");
}

#[test]
fn a_matching_pair_with_group_readable_key_loads() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = paths(dir.path());
    let (cert_pem, key_pem) = bootstrap_ca("Test CA").unwrap();
    write(Path::new(&cert), &cert_pem, 0o644);
    write(Path::new(&key), &key_pem, 0o440);

    let (loaded_cert, loaded_key) = load_provisioned_ca(&cert, &key).expect("loads");

    assert_eq!(loaded_cert, cert_pem);
    assert_eq!(loaded_key, key_pem);
}

#[test]
fn a_world_readable_key_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = paths(dir.path());
    let (cert_pem, key_pem) = bootstrap_ca("Test CA").unwrap();
    write(Path::new(&cert), &cert_pem, 0o644);
    write(Path::new(&key), &key_pem, 0o644);

    let err = load_provisioned_ca(&cert, &key).expect_err("must refuse");

    assert!(matches!(
        err,
        CaMaterialsError::InsecureProvisionedKeyPermissions { .. }
    ));
}

#[test]
fn a_group_writable_key_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = paths(dir.path());
    let (cert_pem, key_pem) = bootstrap_ca("Test CA").unwrap();
    write(Path::new(&cert), &cert_pem, 0o644);
    write(Path::new(&key), &key_pem, 0o660);

    let err = load_provisioned_ca(&cert, &key).expect_err("must refuse");

    assert!(matches!(
        err,
        CaMaterialsError::InsecureProvisionedKeyPermissions { .. }
    ));
}

#[test]
fn a_cert_and_key_from_different_cas_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = paths(dir.path());
    let (cert_pem, _) = bootstrap_ca("CA one").unwrap();
    let (_, other_key_pem) = bootstrap_ca("CA two").unwrap();
    write(Path::new(&cert), &cert_pem, 0o644);
    write(Path::new(&key), &other_key_pem, 0o440);

    let err = load_provisioned_ca(&cert, &key).expect_err("must refuse");

    assert!(matches!(err, CaMaterialsError::Mismatch { .. }));
}

#[test]
fn only_one_of_the_two_files_is_a_partial_state_error() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = paths(dir.path());
    let (cert_pem, _) = bootstrap_ca("Test CA").unwrap();
    write(Path::new(&cert), &cert_pem, 0o644);

    let err = load_provisioned_ca(&cert, &key).expect_err("must refuse");

    assert!(matches!(err, CaMaterialsError::PartialState { .. }));
}
