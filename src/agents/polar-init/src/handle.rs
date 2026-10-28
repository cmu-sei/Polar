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

//! Telling the workload how this pod's escrow key is held.
//!
//! The init container bootstraps the key. If the kernel could hold it, the key is now in the
//! user keyring and only its number is needed: the number is left in a file in the volume the
//! init container and the workload share, and the workload adopts the key by it, so it never
//! holds the key material at all. If the kernel could not, nothing is left, and the workload
//! does what it always did.

use polar_crypto::Custody;
use std::io::Write;
use std::path::Path;

/// Records `custody` at `path` for the workload to find.
///
/// `Kernel`: the number, written whole or not at all (the workload must never read half of
/// it). If it cannot be written the key is unloaded again, since nothing would ever use it,
/// and the error is returned. `Memory`: nothing is written, and a file left by an earlier
/// attempt is removed, so the absence is what says "hold the key yourself".
pub fn record_custody(path: &str, custody: Custody) -> Result<(), String> {
    match custody {
        Custody::Kernel { serial } => write_whole(path, serial).inspect_err(|_| {
            if let Some(key) = polar_kernel_keyring::KeyringSerial::new(serial) {
                polar_kernel_keyring::unload_key(key);
            }
        }),
        // The daemon holds it, and no number is left either.
        Custody::Memory | Custody::Daemon => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("cannot remove the stale key handle {path}: {e}")),
        },
    }
}

fn write_whole(path: &str, serial: i32) -> Result<(), String> {
    let temporary = format!("{path}.tmp");
    let mut file = std::fs::File::create(&temporary)
        .map_err(|e| format!("cannot write the key handle {temporary}: {e}"))?;
    writeln!(file, "{serial}")
        .map_err(|e| format!("cannot write the key handle {temporary}: {e}"))?;
    file.sync_all()
        .map_err(|e| format!("cannot write the key handle {temporary}: {e}"))?;
    std::fs::rename(&temporary, Path::new(path))
        .map_err(|e| format!("cannot place the key handle at {path}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kernel_held_key_leaves_its_number() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escrow-key-handle");
        record_custody(path.to_str().unwrap(), Custody::Kernel { serial: 4242 }).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "4242");
        assert!(
            !dir.path().join("escrow-key-handle.tmp").exists(),
            "no half-written file is left"
        );
    }

    #[test]
    fn a_key_held_in_memory_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escrow-key-handle");
        record_custody(path.to_str().unwrap(), Custody::Memory).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn a_stale_number_is_removed_when_the_key_is_not_in_the_kernel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escrow-key-handle");
        std::fs::write(&path, "999\n").unwrap();
        record_custody(path.to_str().unwrap(), Custody::Memory).unwrap();
        assert!(
            !path.exists(),
            "the absence is what says to hold the key itself"
        );
    }

    #[test]
    fn a_number_that_cannot_be_written_is_an_error_and_the_key_is_unloaded() {
        // The key is real, so that unloading it is observable.
        let der = test_key_der();
        let serial = polar_kernel_keyring::try_load_asymmetric_key("polar-init-handle-test", &der)
            .expect("this machine must support kernel-keyring asymmetric keys for this test");

        let error = record_custody(
            "/nonexistent-directory/escrow-key-handle",
            Custody::Kernel {
                serial: serial.get(),
            },
        )
        .unwrap_err();

        assert!(error.contains("escrow-key-handle"), "{error}");
        assert_eq!(
            polar_kernel_keyring::adopt_asymmetric_key(serial.get(), "polar-init-handle-test"),
            None,
            "nobody will use a key whose number was never passed on"
        );
    }

    fn test_key_der() -> Vec<u8> {
        use rsa::pkcs8::EncodePrivateKey;
        let mut rng = rand::rng();
        rsa::RsaPrivateKey::new(&mut rng, 2048)
            .unwrap()
            .to_pkcs8_der()
            .unwrap()
            .as_bytes()
            .to_vec()
    }
}
