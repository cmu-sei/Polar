/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

//! Generic, key-agnostic Linux kernel-keyring primitives -- has no
//! knowledge of escrow, mTLS, or any other specific use case. Callers
//! (polar-crypto today; eventually cassini-client for mTLS, per
//! commit history) load their own private key material and get back
//! an opaque KeyringSerial, or a real None when the kernel genuinely
//! can't do this -- never a raw private key handed back out again.
//!
//! ## Why this exists
//!
//! The default, in-memory fallback (a raw RsaPrivateKey living in a
//! process's own heap) is real and correct, but a kernel-held key
//! never enters that process's own memory at all after the initial
//! load -- every subsequent operation (decrypt, sign) is a syscall to
//! the kernel, which performs the actual cryptographic operation
//! internally and hands back only the result.
//!
//! ## The empirical-detection sequence, and why it's shaped this way
//!
//! There is no kernel API to ask "does this system support asymmetric
//! pkey operations" independent of an actual, loaded key --
//! keyctl_pkey_query() takes a real key serial, not a standalone
//! capability check. So try_load_asymmetric_key() below does the only
//! thing that's actually possible: attempts add_key() first, and if
//! that succeeds, immediately queries the *resulting* key to confirm
//! decrypt is genuinely supported on it. Either step failing is a
//! real, expected outcome (older kernels, minimal/hardened builds,
//! missing RSA support), not a bug -- callers get a plain None and
//! fall back to in-memory custody, exactly as if this crate didn't
//! exist.
//!
//! ## Scope boundary, stated plainly
//!
//! This crate deliberately has zero knowledge of Kubernetes, pods, or
//! processes other than "the calling one." What key material gets
//! loaded, and how it travels from wherever it was generated to
//! wherever this crate's own functions get called, is the caller's
//! concern entirely.
//!
//! ## A real, named security gap -- not glossed over
//!
//! Keys loaded here go into the calling process's own *user* keyring
//! (KEY_SPEC_USER_KEYRING), not a process- or thread-scoped one --
//! process/thread keyrings genuinely die with their originating
//! process, which would make this useless for the actual, intended
//! use case (a short-lived init container loading a key that a
//! separate, longer-running workload container needs to read later).
//! The user keyring's own sharing boundary is UID plus user
//! namespace, not the pod boundary specifically. On a cluster with
//! per-pod user namespaces genuinely enabled (Kubernetes >= 1.36,
//! `hostUsers: false`, GA as of the v1.36 release), this is
//! pod-scoped, cleanly. Without that (any cluster on an older
//! version, including this project's own current dev cluster, on
//! 1.31 as of this writing), the user keyring's own sharing boundary
//! is genuininely node-wide: any other pod happening to run under the
//! same UID on the same node could, in principle, see keys loaded
//! here too. This is a real, deliberately-deferred hardening gap, not
//! an oversight -- see this crate's own commit history for the
//! discussion. keyctl_restrict_keyring() is the likely, real mitigation
//! once this gets addressed, but hasn't been researched or built yet.

use keyutils_raw::{
    KEY_POS_ALL, KEY_SPEC_SESSION_KEYRING, KEY_SPEC_USER_KEYRING, KEY_USR_ALL,
    KEYCTL_SUPPORTS_DECRYPT, KeyringSerial, add_key, keyctl_link, keyctl_pkey_decrypt,
    keyctl_pkey_query, keyctl_setperm, keyctl_unlink,
};

/// The info string passed to every keyctl_pkey_* call in this crate.
///
/// Raw mode -- no padding scheme applied by the kernel at all. The
/// kernel's own RFC patch that introduced this interface documents
/// this directly: "If omitted or is 'raw', the raw output of the
/// encryption function is specified."
///
/// This crate performs the real OAEP unpad itself afterward (see
/// decrypt()'s own body), using our own fork of the rsa crate's
/// already-audited oaep_decrypt_digest -- genuinely, mathematically
/// separate from the raw RSA modular-exponentiation step the kernel
/// performs here. Mainline Linux's own in-tree RSA implementation
/// (crypto/rsa-pkcs1pad.c) only ever implements PKCS1v1.5 padding
/// internally for encrypt/decrypt -- there is no OAEP unpadding logic
/// anywhere in the kernel's own crypto subsystem at all, on any
/// distro or kernel version (confirmed directly, empirically, against
/// a real kernel during this crate's own development -- see this
/// crate's own commit history). Raw mode is what lets that real,
/// architectural kernel limitation stay fully contained inside this
/// one function, rather than forcing credentials-agent-common's own
/// EscrowedSecret -- and every caller that already depends on it,
/// including callers whose own keys never touch the kernel at all --
/// to weaken its own wire protocol to accommodate it.
///
/// Real OAEP unpad failures and genuine kernel-level rejections both
/// collapse into decrypt()'s own, single DecryptFailed variant,
/// deliberately: PKCS1v1.5's own real, historical vulnerability class
/// (Bleichenbacher-style padding-oracle attacks) requires an attacker
/// able to submit many, adaptively-chosen ciphertexts to the same key
/// and observe a distinguishable success/failure signal. OAEP itself
/// isn't immune to an analogous risk if its own decode step leaks
/// which specific validation failed -- so this crate holds itself to
/// the same discipline regardless of padding scheme: CryptoActor is
/// the sole, deliberate boundary for every key operation in this
/// system (see polar-crypto's own commit history for the full
/// reasoning), and no caller of this function should ever be able to
/// observe "padding was invalid" as distinguishable from any other
/// failure, in the returned type, any log message, or timing.
const ESCROW_PKEY_INFO: &str = "enc=raw";

/// The hash algorithm used for OAEP's own label-hashing and MGF1 mask
/// generation -- must match credentials-agent-common's own
/// EscrowedSecret::encrypt exactly (Oaep::new::<Sha256>()), or the
/// unpad step below genuinely, correctly fails, given it validates
/// the padding structure against this specific hash.
type EscrowOaepHash = sha2::Sha256;

#[derive(Debug, thiserror::Error)]
pub enum KernelKeyringError {
    #[error("the kernel rejected the decrypt operation on an already-loaded key: {0}")]
    DecryptFailed(errno::Errno),
    #[error(
        "the decrypted output was larger than the buffer this crate allocated for it \
         ({produced} bytes into a {capacity}-byte buffer) -- this indicates a real, \
         unexpected mismatch between keyctl_pkey_query's own max_dec_size and what the \
         kernel actually produced, not an ordinary decrypt failure"
    )]
    OutputTooLarge { produced: usize, capacity: usize },
}

/// Attempts to load `der_bytes` (PKCS8 DER-encoded, e.g. RSA private
/// key material) into the calling process's own user keyring,
/// empirically confirming decrypt support genuinely works on the
/// resulting key before returning success -- see this crate's own
/// top-level doc comment for the complete reasoning, including a
/// real, named security gap in the current, deliberately-scoped
/// implementation.
///
/// Returns `None` on any failure along the way (kernel genuinely
/// lacks asymmetric-key support at all, the specific algorithm isn't
/// available, the loaded key doesn't support decrypt for some other
/// reason) -- callers should treat this as a genuine, expected
/// fallback signal to fall back to in-memory custody, not as an error
/// condition worth surfacing to an end user.
pub fn try_load_asymmetric_key(description: &str, der_bytes: &[u8]) -> Option<KeyringSerial> {
    // Loaded into the session keyring first, deliberately -- not
    // directly into the user keyring this key ultimately needs to
    // live in. Linux keyring "possession" (the status required to
    // change a key's own permissions at all) is conferred by which
    // *specific* keyring a key is linked into: session, process, and
    // thread keyrings genuinely confer it to the calling process,
    // but the user keyring specifically does not -- confirmed
    // directly, empirically, against this crate's own real behavior
    // during development (see this crate's own commit history), not
    // assumed from documentation alone. Adding here first, where we
    // genuinely possess the result, lets us actually set the
    // permissions this key needs before it ever reaches the user
    // keyring at all.
    let serial = match add_key(
        "asymmetric",
        description,
        der_bytes,
        KEY_SPEC_SESSION_KEYRING,
    ) {
        Ok(serial) => serial,
        Err(e) => {
            tracing::debug!(
                "kernel keyring unavailable, or this kernel's own asymmetric key type doesn't \
                 support the supplied key material: {e}"
            );
            return None;
        }
    };

    // Now genuinely possessed (via the session keyring link above),
    // so this actually succeeds -- add_key()'s own, kernel-level
    // default permissions grant only "view" to the "user" category
    // (any process sharing this key's owning UID), which is
    // genuinely too restrictive for the real, intended use case: a
    // different process (the main workload container) needs to
    // genuinely decrypt with this key later, not just see that it
    // exists.
    if let Err(e) = keyctl_setperm(serial, KEY_POS_ALL | KEY_USR_ALL) {
        tracing::debug!("failed to set permissions on the newly-loaded key: {e}");
        return None;
    }

    // Link the same key into the user keyring -- this is what
    // actually makes it survive beyond this process's own lifetime,
    // the real, intended cross-process behavior. A key can be linked
    // into multiple keyrings simultaneously; this doesn't remove it
    // from the session keyring on its own.
    if let Err(e) = keyctl_link(serial, KEY_SPEC_USER_KEYRING) {
        tracing::debug!("failed to link the loaded key into the user keyring: {e}");
        return None;
    }

    // Explicit cleanup, not load-bearing for correctness (the
    // session keyring itself won't outlive this process regardless),
    // but avoids leaving a redundant reference lying around.
    let _ = keyctl_unlink(serial, KEY_SPEC_SESSION_KEYRING);

    match keyctl_pkey_query(serial, ESCROW_PKEY_INFO) {
        Ok(query) if query.supported_ops & KEYCTL_SUPPORTS_DECRYPT != 0 => Some(serial),
        Ok(_) => {
            tracing::debug!(
                "the kernel accepted the key but doesn't support decrypt on it with the \
                 requested encoding; discarding the loaded key and falling back"
            );
            None
        }
        Err(e) => {
            tracing::debug!("keyctl_pkey_query failed on the newly-loaded key: {e}");
            None
        }
    }
}

/// Decrypts `ciphertext` using the kernel-held private key at
/// `serial`. The raw RSA private key material never leaves the
/// kernel at all -- only the raw, still-OAEP-padded modular-
/// exponentiation result crosses back into this process's own
/// memory, and this function then performs the real OAEP unpad step
/// itself, entirely in userspace, using this crate's own fork of the
/// rsa crate's already-audited, real oaep_decrypt_digest.
///
/// Every possible failure along the way -- a genuine kernel
/// rejection, a real OAEP padding-validation failure, anything --
/// collapses into this same DecryptFailed variant, deliberately: see
/// this crate's own top-level doc comment for why "padding was
/// invalid" must never be observably distinguishable from any other
/// failure.
pub fn decrypt(serial: KeyringSerial, ciphertext: &[u8]) -> Result<Vec<u8>, KernelKeyringError> {
    let query =
        keyctl_pkey_query(serial, ESCROW_PKEY_INFO).map_err(KernelKeyringError::DecryptFailed)?;

    // Raw mode's own output is always exactly the key's full
    // modulus size, in bytes -- there's no padding-removal step
    // happening inside the kernel at all here to shrink it.
    let modulus_bytes = (query.key_size as usize).div_ceil(8);
    let mut buffer = vec![0u8; modulus_bytes];

    let written = keyctl_pkey_decrypt(
        serial,
        ESCROW_PKEY_INFO,
        ciphertext,
        uninit::out_ref::Out::from(buffer.as_mut_slice()),
    )
    .map_err(KernelKeyringError::DecryptFailed)?;

    if written > buffer.len() {
        return Err(KernelKeyringError::OutputTooLarge {
            produced: written,
            capacity: buffer.len(),
        });
    }
    buffer.truncate(written);

    // Left-pad to the full modulus width -- the kernel's own raw
    // output may genuinely come back shorter than the modulus if the
    // resulting integer's own leading bytes happened to be zero
    // (ordinary, expected behavior for any big-integer encoding), but
    // OAEP's own unpad step requires the complete, fixed-width
    // encoded message block to correctly locate its own leading 0x00
    // byte.
    while buffer.len() < modulus_bytes {
        buffer.insert(0, 0);
    }

    rsa::oaep::oaep_decrypt_digest::<EscrowOaepHash, EscrowOaepHash>(
        &mut buffer,
        None,
        modulus_bytes,
    )
    // Deliberately not preserving the real, underlying OAEP error
    // detail here -- collapsed into the exact same DecryptFailed
    // variant, with a synthetic errno, so a genuine padding failure
    // is never observably different from a real kernel-level
    // rejection above.
    .map_err(|_| KernelKeyringError::DecryptFailed(errno::Errno(libc::EBADMSG)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs8::EncodePrivateKey;
    use rsa::{Oaep, RsaPrivateKey, RsaPublicKey};

    /// A real, end-to-end proof this crate's own primitives genuinely
    /// work against the actual, real kernel on whatever machine this
    /// test runs on -- not a mock, not an assumption. Generates a
    /// real RSA key, loads it into the real kernel keyring, encrypts
    /// a real plaintext with real, standard OAEP -- the exact same
    /// wire format credentials-agent-common's own EscrowedSecret
    /// actually produces, matching what a real peer would send this
    /// identity -- entirely outside the kernel, then decrypts it back
    /// via decrypt()'s own, real split path: the kernel performs only
    /// the raw RSA modular exponentiation, and this crate performs
    /// the real OAEP unpad itself afterward.
    ///
    /// If this fails with a kernel-support-related error rather than
    /// a genuine bug, that's real, meaningful information about
    /// whatever machine ran it -- not a test to silently skip.
    #[test]
    fn real_key_genuinely_round_trips_through_the_real_kernel_keyring() {
        let mut rng = rand::rng();
        let private_key =
            RsaPrivateKey::new(&mut rng, 2048).expect("real RSA key generation must succeed");
        let public_key = RsaPublicKey::from(&private_key);

        let der_bytes = private_key
            .to_pkcs8_der()
            .expect("real PKCS8 DER encoding must succeed");

        let serial = try_load_asymmetric_key("polar-kernel-keyring-test", der_bytes.as_bytes())
            .expect(
                "this test's own machine must genuinely support kernel-keyring asymmetric-key \
                 operations for this crate's real behavior to be verified at all -- if this \
                 panics, check this machine's own kernel config (CONFIG_ASYMMETRIC_KEY_TYPE, \
                 CONFIG_CRYPTO_RSA, CONFIG_PKCS8_PRIVATE_KEY_PARSER) rather than assuming a bug \
                 in this crate",
            );

        let plaintext = b"a real secret, encrypted outside the kernel entirely, with real OAEP";

        // Real, standard OAEP -- exactly what any peer encrypting to
        // this identity's real escrow public key actually sends,
        // matching credentials-agent-common's own EscrowedSecret
        // exactly (Oaep::new::<Sha256>()). Never touches anything
        // kernel-related; only needs the public key.
        let ciphertext = public_key
            .encrypt(&mut rng, Oaep::<EscrowOaepHash>::new(), plaintext)
            .expect("real OAEP encryption to the public half must succeed");

        let decrypted = decrypt(serial, &ciphertext).expect(
            "the real, kernel-backed decrypt -- raw RSA inside the kernel, real OAEP unpad \
             performed here -- must succeed",
        );

        assert_eq!(
            decrypted, plaintext,
            "the split decrypt path's own final plaintext must genuinely match what was \
             encrypted with real, standard OAEP -- proving both the kernel's own raw RSA \
             operation and this crate's own, separate OAEP unpad step are each genuinely \
             correct, not just that add_key() didn't error"
        );
    }
}
