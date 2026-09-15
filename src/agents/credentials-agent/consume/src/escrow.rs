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

//! A real, persisted keypair -- generated once, ever, then loaded on
//! every subsequent restart, matching cert-issuer's own
//! load_or_bootstrap_ca pattern (check if the file exists, load if
//! so, generate and persist if not). Without this, a fresh keypair
//! generated on every restart would make anything encrypted to a
//! prior process's key permanently unreadable the moment that process
//! restarted -- the gap this file's own doc comment previously
//! flagged as real, tracked follow-up work.
//!
//! Still genuinely incomplete relative to credentials-agent-architecture.md's
//! full secret-escrow design: no rotation, no identity-distribution
//! topic pair, and the private key is currently unused (nothing yet
//! performs the decrypt/re-encrypt relay this key is ultimately for).
//! Held anyway, rather than discarded, specifically so that work
//! reuses this same persisted keypair instead of a second, wasted
//! generation.
//!
//! Deliberately a separate keypair from this agent's own mTLS client
//! certificate -- reusing one asymmetric keypair across two different
//! cryptographic protocols is a real cryptographic-engineering smell
//! even when nothing obviously breaks from doing so.

use rsa::RsaPrivateKey;
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};

pub const CREDENTIALS_ESCROW_KEY_PATH_ENV: &str = "CREDENTIALS_ESCROW_KEY_PATH";
pub const CREDENTIALS_ESCROW_KEY_PATH_DEFAULT: &str = "/var/lib/credentials-agent/escrow-key.pem";

#[derive(Debug, thiserror::Error)]
pub enum EscrowKeypairError {
    #[error("failed to generate escrow keypair: {0}")]
    Generation(String),
    #[error("failed to encode escrow key: {0}")]
    Encoding(String),
    #[error("failed to read existing escrow key at {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to parse existing escrow key at {path}: {detail}")]
    Parse { path: String, detail: String },
    #[error("failed to write escrow key to {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
    #[cfg(unix)]
    #[error("failed to set escrow key file permissions at {path}: {source}")]
    Permissions {
        path: String,
        source: std::io::Error,
    },
}

pub struct EscrowKeypair {
    private_key: RsaPrivateKey,
    public_key_pem: String,
}

impl EscrowKeypair {
    fn from_private_key(private_key: RsaPrivateKey) -> Result<Self, EscrowKeypairError> {
        let public_key = private_key.to_public_key();
        let public_key_pem =
            rsa::pkcs8::EncodePublicKey::to_public_key_pem(&public_key, LineEnding::LF)
                .map_err(|e| EscrowKeypairError::Encoding(e.to_string()))?;
        Ok(Self {
            private_key,
            public_key_pem,
        })
    }

    pub fn generate() -> Result<Self, EscrowKeypairError> {
        let mut rng = rand::rng();
        let private_key = RsaPrivateKey::new(&mut rng, 4096)
            .map_err(|e| EscrowKeypairError::Generation(e.to_string()))?;
        Self::from_private_key(private_key)
    }

    /// Loads a persisted keypair from `path` if it exists, or
    /// generates a new one and persists it there if not.
    pub fn load_or_generate(path: &str) -> Result<Self, EscrowKeypairError> {
        if std::path::Path::new(path).exists() {
            Self::load(path)
        } else {
            let keypair = Self::generate()?;
            keypair.persist(path)?;
            Ok(keypair)
        }
    }

    fn load(path: &str) -> Result<Self, EscrowKeypairError> {
        let pem = std::fs::read_to_string(path).map_err(|e| EscrowKeypairError::Read {
            path: path.to_string(),
            source: e,
        })?;
        let private_key =
            RsaPrivateKey::from_pkcs8_pem(&pem).map_err(|e| EscrowKeypairError::Parse {
                path: path.to_string(),
                detail: e.to_string(),
            })?;
        Self::from_private_key(private_key)
    }

    fn persist(&self, path: &str) -> Result<(), EscrowKeypairError> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent).map_err(|e| EscrowKeypairError::Write {
                path: path.to_string(),
                source: e,
            })?;
        }
        let pem = self
            .private_key
            .to_pkcs8_pem(LineEnding::LF)
            .map_err(|e| EscrowKeypairError::Encoding(e.to_string()))?;
        std::fs::write(path, pem.as_bytes()).map_err(|e| EscrowKeypairError::Write {
            path: path.to_string(),
            source: e,
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path)
                .map_err(|e| EscrowKeypairError::Permissions {
                    path: path.to_string(),
                    source: e,
                })?
                .permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(path, perms).map_err(|e| {
                EscrowKeypairError::Permissions {
                    path: path.to_string(),
                    source: e,
                }
            })?;
        }

        Ok(())
    }

    pub fn public_key_pem(&self) -> &str {
        &self.public_key_pem
    }

    /// The genuine reason this keypair exists in the first place --
    /// used by the decrypt-reencrypt-destroy relay to actually
    /// decrypt what's held in escrow, transiently, at the exact
    /// moment a specific, authorized consumer needs it.
    pub fn private_key(&self) -> &RsaPrivateKey {
        &self.private_key
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Cached across every test that needs *a* valid keypair but
    /// doesn't care about its specific value -- RSA generation is
    /// expensive (>100ms), matching cert-issuer's own established
    /// caching convention for exactly this reason.
    pub(crate) fn test_keypair() -> std::sync::Arc<EscrowKeypair> {
        use std::sync::{Arc, OnceLock};
        static KEYPAIR: OnceLock<Arc<EscrowKeypair>> = OnceLock::new();
        KEYPAIR
            .get_or_init(|| Arc::new(EscrowKeypair::generate().unwrap()))
            .clone()
    }

    #[test]
    fn generates_a_real_pem_encoded_public_key() {
        let keypair = EscrowKeypair::generate().unwrap();
        assert!(
            keypair
                .public_key_pem()
                .starts_with("-----BEGIN PUBLIC KEY-----")
        );
        assert!(keypair.public_key_pem().ends_with("-----END PUBLIC KEY-----\n"));
    }

    #[test]
    fn two_generated_keypairs_are_genuinely_different() {
        let a = EscrowKeypair::generate().unwrap();
        let b = EscrowKeypair::generate().unwrap();
        assert_ne!(a.public_key_pem(), b.public_key_pem());
    }

    #[test]
    fn load_or_generate_persists_a_new_keypair_when_none_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escrow-key.pem");
        assert!(!path.exists());

        let keypair = EscrowKeypair::load_or_generate(path.to_str().unwrap()).unwrap();

        assert!(path.exists(), "a new keypair must be persisted to disk");
        assert!(!keypair.public_key_pem().is_empty());
    }

    #[test]
    fn load_or_generate_returns_the_same_key_on_a_second_call() {
        // The real property this whole file exists for: a second
        // "restart" against the same path must load the identical
        // key, not generate a new, unrelated one.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escrow-key.pem");

        let first = EscrowKeypair::load_or_generate(path.to_str().unwrap()).unwrap();
        let second = EscrowKeypair::load_or_generate(path.to_str().unwrap()).unwrap();

        assert_eq!(first.public_key_pem(), second.public_key_pem());
    }

    #[cfg(unix)]
    #[test]
    fn persisted_key_file_has_mode_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escrow-key.pem");
        EscrowKeypair::load_or_generate(path.to_str().unwrap()).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "persisted escrow key must be mode 0600");
    }

    #[test]
    fn load_or_generate_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("dirs").join("escrow-key.pem");
        assert!(!path.parent().unwrap().exists());

        EscrowKeypair::load_or_generate(path.to_str().unwrap()).unwrap();

        assert!(path.exists());
    }
}
