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

//! In-process CA backed by rcgen.
//!
//! v1 of the cert issuer holds a CA root keypair in memory and signs
//! CSRs directly. There is no external CA process. The `CaClient`
//! trait is the seam — handler tests substitute a mock; production
//! plugs in `RcgenCaClient`; future implementations could plug in
//! step-ca, Vault PKI, or any other backend.
//!
//! # Why in-process
//!
//! The dependency surface analysis in the architecture spec calls
//! out the CA as one of the cert issuer's runtime dependencies. By
//! holding the CA in-process, we collapse that dependency: the cert
//! issuer's startup loads the CA root from the secrets backend (a
//! mounted Kubernetes secret in production, a local file in dev)
//! and serves issuance directly. No CA daemon to run, monitor,
//! upgrade, or back up. No wire format to validate against an
//! external service.
//!
//! The tradeoff is that compromise of the cert issuer is compromise
//! of the CA root. For an internal-only PKI signing certs that only
//! agents on the Polar cluster trust, this is acceptable — the
//! blast radius is bounded by what those certs can do, which is
//! talk to Cassini and the credential agent. If you're using these
//! certs to anchor any external trust relationship, you would
//! instead want a separate CA process; in that case the original
//! `StepCaClient` design pattern (a remote CA reached via HTTP) is
//! the right shape, just not what we need right now.
//!
//! # CA root rotation
//!
//! rcgen-issued certs have whatever validity bounds we tell them
//! to. The CA root itself has its own validity (configured at
//! bootstrap, typically 5-10 years). Rotation is operator-driven:
//! generate a new root, deploy it alongside the old one for a
//! transition window, swap the cert issuer's loaded key. The
//! current implementation supports loading a single CA at a time;
//! cross-signing for transition would be a v1.x addition.

use cert_issuer_common::CertType;
use rcgen::{
    CertificateSigningRequestParams, ExtendedKeyUsagePurpose, Issuer, KeyPair, KeyUsagePurpose,
};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use time::OffsetDateTime;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CaIssueRequest {
    /// PEM-encoded CSR. Parsed and validated by the `CaClient`
    /// before signing.
    pub csr_pem: String,
    /// SANs to encode in the issued cert. The first entry is always
    /// the identity SAN derived from the OIDC token and verified
    /// against the CSR. Subsequent entries are extra SANs validated
    /// by the handler before this request is constructed.
    pub sans: Vec<String>,
    /// Cert lifetime. The issued cert's `notAfter` is computed as
    /// `now + lifetime`.
    pub lifetime: Duration,
    pub cert_type: CertType,
    /// Subject DN fields to embed, already merged and validated by
    /// the handler (see handler::merge_subject_dn). Every `Some`
    /// field becomes a real X.520 Subject DN attribute (RFC 4519) --
    /// standard, not a Polar-proprietary encoding, unlike how
    /// Permission rides in SANs today.
    pub subject_dn: crate::handler::MergedSubjectDn,
    /// How it came to be signed, for the record (`crate::record`).
    pub path: cert_issuer_common::record::IssuePath,
    /// The pod instance it is for, when a token said so, for the record.
    pub instance_binding: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IssuedCert {
    pub certificate_pem: String,
    pub ca_chain_pem: String,
    pub serial_number: String,
    pub not_after: OffsetDateTime,
}

#[derive(Debug, Clone, Error)]
pub enum CaError {
    /// CSR parse failure or other input-level error. The closest
    /// analogue to step-ca's "client-side problem"; surfaces as
    /// 400 to the init container (via the handler).
    #[error("malformed input: {0}")]
    Malformed(String),
    /// Signing operation failed. Indicates either a programming bug
    /// or a corrupted CA keypair. Surfaces as 503 since a transient
    /// retry is probably the operator's first move.
    #[error("signing failed: {0}")]
    SigningFailed(String),
}

#[async_trait::async_trait]
pub trait CaClient: Send + Sync {
    async fn issue(&self, req: CaIssueRequest) -> Result<IssuedCert, CaError>;
}

/// The key usage of an escrow certificate, by the kind of key a secret is sealed to (polar-escrow's `Suite`).
fn escrow_key_usage(algorithm: &rcgen::SignatureAlgorithm) -> Result<KeyUsagePurpose, CaError> {
    if algorithm == &rcgen::PKCS_ECDSA_P256_SHA256 {
        Ok(KeyUsagePurpose::KeyAgreement)
    } else if [
        &rcgen::PKCS_RSA_SHA256,
        &rcgen::PKCS_RSA_SHA384,
        &rcgen::PKCS_RSA_SHA512,
    ]
    .contains(&algorithm)
    {
        Ok(KeyUsagePurpose::KeyEncipherment)
    } else {
        Err(CaError::Malformed(
            "an escrow certificate is for a P-256 or an RSA key: no secret is sealed to this one"
                .to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// rcgen-backed implementation
// ---------------------------------------------------------------------------

/// In-process CA backed by rcgen.
///
/// The CA's keypair and root certificate are loaded once at
/// construction and held for the service's lifetime. Concurrent
/// issuance requests share the same `Issuer` via `Arc`; rcgen's
/// signing operation is thread-safe.
pub struct RcgenCaClient {
    issuer: Arc<Issuer<'static, KeyPair>>,
    /// PEM of the CA cert, returned in every `IssuedCert.ca_chain_pem`
    /// so the init container can write it to the shared volume for
    /// the workload to use as a trust root.
    ca_chain_pem: String,
}

impl RcgenCaClient {
    /// Construct a new CA client by loading the CA cert and keypair
    /// from PEM.
    ///
    /// `ca_cert_pem` is the CA's self-signed certificate. It is
    /// re-emitted as the CA chain in every issuance response.
    /// `ca_key_pem` is the CA's private key in PKCS#8 PEM form.
    /// The key is held in memory for the service's lifetime; we
    /// never log, serialize, or write it.
    pub fn new(ca_cert_pem: &str, ca_key_pem: &str) -> Result<Self, String> {
        let key_pair = KeyPair::from_pem(ca_key_pem).map_err(|e| format!("CA private key: {e}"))?;

        let issuer = Issuer::from_ca_cert_pem(ca_cert_pem, key_pair)
            .map_err(|e| format!("invalid CA cert/key: {e}"))?;
        Ok(Self {
            issuer: Arc::new(issuer),
            ca_chain_pem: ca_cert_pem.to_string(),
        })
    }
}

#[async_trait::async_trait]
impl CaClient for RcgenCaClient {
    async fn issue(&self, req: CaIssueRequest) -> Result<IssuedCert, CaError> {
        let mut csr = CertificateSigningRequestParams::from_pem(&req.csr_pem)
            .map_err(|e| CaError::Malformed(format!("CSR parse: {e}")))?;

        // Override the SANs from the CSR with the server-validated list.
        // The handler has already verified the first SAN matches the token
        // identity and validated any extras. We don't trust the CSR's SAN
        // list directly — we trust what the handler computed.
        csr.params.subject_alt_names = req
            .sans
            .iter()
            .map(|s| {
                let ia5 = s
                    .clone()
                    .try_into()
                    .map_err(|e| CaError::Malformed(format!("SAN: {e}")))?;
                // A Permission's (`p:`) or Capability's (`c:`) own SAN-string
                // form is not hostname-shaped -- it's deliberately terse,
                // colon-delimited metadata (p:s2s:cassini.cluster.local,
                // c:provision-account), which DnsName's own semantic
                // expectations don't fit, even though the underlying
                // Ia5String type itself permits the characters. URI carries
                // no such hostname expectation; a colon is ordinary,
                // defining syntax there. Every other extra SAN keeps its
                // existing DnsName treatment.
                // A group (`g:`) is the same kind of metadata.
                if s.starts_with("p:") || s.starts_with("c:") || s.starts_with("g:") {
                    Ok(rcgen::SanType::URI(ia5))
                } else {
                    Ok(rcgen::SanType::DnsName(ia5))
                }
            })
            .collect::<Result<Vec<_>, CaError>>()?;

        // Subject DN fields, beyond the CommonName rcgen's own
        // CSR-params conversion already sets. Real X.520 attribute
        // types (RFC 4519 section 2) -- readable by any standard
        // X.509 tool, not just Polar's own, unlike the SAN-riding
        // Permission convention above.
        const OID_GIVEN_NAME: [u64; 4] = [2, 5, 4, 42];
        const OID_SURNAME: [u64; 4] = [2, 5, 4, 4];
        const OID_TITLE: [u64; 4] = [2, 5, 4, 12];
        const OID_TELEPHONE_NUMBER: [u64; 4] = [2, 5, 4, 20];
        // The account's own username and email: `uid` (RFC 4519) and `mail`
        // (RFC 4524), both standard, both under the same 0.9.2342.19200300.100.1
        // arc. Authoritative -- see handler::MergedSubjectDn.
        const OID_UID: [u64; 7] = [0, 9, 2342, 19200300, 100, 1, 1];
        const OID_MAIL: [u64; 7] = [0, 9, 2342, 19200300, 100, 1, 3];

        if let Some(v) = &req.subject_dn.given_name {
            csr.params.distinguished_name.push(
                rcgen::DnType::CustomDnType(OID_GIVEN_NAME.to_vec()),
                v.clone(),
            );
        }
        if let Some(v) = &req.subject_dn.surname {
            csr.params
                .distinguished_name
                .push(rcgen::DnType::CustomDnType(OID_SURNAME.to_vec()), v.clone());
        }
        if let Some(v) = &req.subject_dn.title {
            csr.params
                .distinguished_name
                .push(rcgen::DnType::CustomDnType(OID_TITLE.to_vec()), v.clone());
        }
        if let Some(v) = &req.subject_dn.telephone_number {
            csr.params.distinguished_name.push(
                rcgen::DnType::CustomDnType(OID_TELEPHONE_NUMBER.to_vec()),
                v.clone(),
            );
        }
        // rcgen starts from the CSR's own Subject, so whatever the CSR claims
        // for these two authoritative attributes would otherwise survive into
        // the certificate. Clear them first; only the caller's values below
        // may ever be written.
        csr.params
            .distinguished_name
            .remove(rcgen::DnType::CustomDnType(OID_UID.to_vec()));
        csr.params
            .distinguished_name
            .remove(rcgen::DnType::CustomDnType(OID_MAIL.to_vec()));

        if let Some(v) = &req.subject_dn.username {
            csr.params
                .distinguished_name
                .push(rcgen::DnType::CustomDnType(OID_UID.to_vec()), v.clone());
        }
        if let Some(v) = &req.subject_dn.email {
            // `mail` is an IA5String, not the UTF8String rcgen would pick
            // by default for a plain string value.
            let ia5 = v
                .clone()
                .try_into()
                .map_err(|e| CaError::Malformed(format!("email: {e}")))?;
            csr.params.distinguished_name.push(
                rcgen::DnType::CustomDnType(OID_MAIL.to_vec()),
                rcgen::DnValue::Ia5String(ia5),
            );
        }

        let not_before = OffsetDateTime::now_utc();
        let not_after = not_before + req.lifetime;

        let eku = match req.cert_type {
            CertType::Client => vec![ExtendedKeyUsagePurpose::ClientAuth],
            CertType::Server => vec![
                ExtendedKeyUsagePurpose::ServerAuth,
                ExtendedKeyUsagePurpose::ClientAuth, // keep both — Cassini connects to peers too
            ],
            // The application layer's certificate (whom a secret is encrypted to), never the transport layer's
            // (who may hold a connection): emailProtection, the purpose of a certificate messages are encrypted to,
            // which no TLS stack takes for a client or a server. With no extended key usage at all it would be taken
            // for every purpose, client and server authentication included.
            CertType::Service => vec![ExtendedKeyUsagePurpose::EmailProtection],
            // Present, and not a TLS purpose: a certificate with no extended key usage at all is taken for any
            // purpose, client and server authentication included.
            CertType::Signing => vec![ExtendedKeyUsagePurpose::Other(
                cert_issuer_common::OID_DOCUMENT_SIGNING.to_vec(),
            )],
        };
        csr.params.extended_key_usages = eku;
        csr.params.key_usages = match req.cert_type {
            CertType::Client | CertType::Server | CertType::Signing => {
                vec![KeyUsagePurpose::DigitalSignature]
            }
            // What a secret is sealed to, not anything that signs: by key agreement to a P-256 key (HPKE), or by
            // wrapping its key to an RSA key (RSA-OAEP); see polar-escrow's `Suite`. A key no secret can be sealed
            // to is no one's escrow key.
            CertType::Service => vec![escrow_key_usage(csr.public_key.algorithm())?],
        };
        csr.params.not_before = not_before;
        csr.params.not_after = not_after;

        let cert = csr
            .signed_by(&self.issuer)
            .map_err(|e| CaError::SigningFailed(format!("rcgen sign: {e}")))?;

        let certificate_pem = cert.pem();
        let serial_number = cert_issuer_common::serial::serial_from_pem(&certificate_pem)
            .unwrap_or_else(|| "unknown".to_string());

        Ok(IssuedCert {
            certificate_pem,
            ca_chain_pem: self.ca_chain_pem.clone(),
            serial_number,
            not_after,
        })
    }
}

// ---------------------------------------------------------------------------
// CA bootstrap helper
// ---------------------------------------------------------------------------

// bootstrap_ca lives in cert-issuer-common (ca_root.rs) so the admin
// tool can generate a CA without depending on this crate; re-exported
// here so callers of `ca::bootstrap_ca` are unchanged.
pub use cert_issuer_common::ca_root::bootstrap_ca;

// ---------------------------------------------------------------------------
// Load-or-bootstrap entry point
// ---------------------------------------------------------------------------

/// Error states for the CA materials state machine. These are
/// surfaced as `anyhow::Error` to the caller, but the structured
/// type makes the test assertions cleaner.
#[derive(Debug, Error)]
pub enum CaMaterialsError {
    /// Exactly one of the two files exists. This is a partial state
    /// that almost always indicates an operational problem (a
    /// half-finished restore, a half-finished delete, a wrong path
    /// in config, a volume that mounted partially). Silently
    /// regenerating would either replace whichever file exists with
    /// a fresh-but-mismatched counterpart, or — worse — keep the
    /// stale file and pair it with a freshly generated companion
    /// that doesn't match. Both outcomes break things in confusing
    /// ways. Bail out instead.
    #[error("inconsistent CA materials: {existing} exists but {missing} does not")]
    PartialState { existing: String, missing: String },

    /// The key file exists but is readable by users other than the
    /// cert issuer's own. Zero-trust: we don't use a key the host
    /// could share with another process. The operator must fix the
    /// permissions before we'll proceed.
    #[error("CA key {path} has insecure permissions {mode:o}; expected 0600")]
    InsecureKeyPermissions { path: String, mode: u32 },

    /// The cert and key both load and parse, but the key's public
    /// half doesn't match the cert's embedded public key. Indicates
    /// someone replaced one but not the other. Hard error rather
    /// than silently regenerating, since the existing cert may
    /// still be trusted by clients in the field.
    #[error("CA cert and key do not match: {detail}")]
    Mismatch { detail: String },

    /// Provisioned mode only: neither file exists. The CA is supposed
    /// to have been generated ahead of time by an operator; this
    /// service never makes one up in that mode, because a lost or
    /// mis-mounted Secret would otherwise quietly become a brand-new
    /// root that no client trusts.
    #[error("no provisioned CA found: neither {cert} nor {key} exists")]
    NotProvisioned { cert: String, key: String },

    /// Provisioned mode only: the key is writable by its group or
    /// accessible to others. Group-read is allowed because a mounted
    /// Secret is typically shared with the pod through fsGroup.
    #[error(
        "provisioned CA key {path} has insecure permissions {mode:o}; group-write and any access for others are forbidden"
    )]
    InsecureProvisionedKeyPermissions { path: String, mode: u32 },

    /// Catch-all for parse failures, IO errors writing, etc.
    /// Wraps the underlying error message because the underlying
    /// types are heterogeneous (io::Error, rcgen::Error, etc.) and
    /// the operator cares about the message, not the type.
    #[error("{0}")]
    Other(String),
}

/// Load CA materials from disk, generating fresh ones if they
/// don't exist.
///
/// # State machine
///
/// This function handles the full set of states the on-disk CA
/// files can be in:
///
/// | Cert file | Key file | Action                                |
/// |-----------|----------|---------------------------------------|
/// | exists    | exists   | Validate and load. Mismatch → error.  |
/// | missing   | missing  | Bootstrap a fresh CA, write both.     |
/// | exists    | missing  | Error: partial state.                 |
/// | missing   | exists   | Error: partial state.                 |
///
/// Within "both exist," we additionally check:
/// - The key file's permissions are 0600 (Unix only; on other
///   platforms, this check is skipped because file modes don't
///   carry the same meaning).
/// - The cert and key parse cleanly.
/// - The key's public half matches the cert's embedded public key.
///
/// Any of these failing is an error that surfaces to the operator
/// rather than triggering silent regeneration.
///
/// # Why not "regenerate on any error"
///
/// The zero-trust framing is: the cert issuer owns its CA root.
/// "Owning" means a key the cert issuer wrote, on a path the cert
/// issuer controls, with permissions only the cert issuer can read.
/// Anything that violates one of those properties is an operator
/// problem — silently regenerating would either mask a real
/// security issue (someone else writing into the directory) or
/// destroy CA materials that downstream clients are still trusting
/// (a misconfigured volume mount that points at the wrong path on
/// restart). Failing loudly is the right default; the operator
/// can resolve the underlying issue and restart.
///
/// The one case we do recover from is "neither file exists" — that
/// IS a legitimate first-run state, and generating fresh materials
/// is the only thing that makes sense. The caller should be
/// prepared for first-run latency: rcgen keygen takes tens of ms,
/// not seconds.
///
/// # Returns
///
/// A `(cert_pem, key_pem)` tuple. The caller passes these to
/// `RcgenCaClient::new`. The function does not retain copies.
pub fn load_or_bootstrap_ca(
    cert_path: &str,
    key_path: &str,
    bootstrap_common_name: &str,
) -> Result<(String, String), CaMaterialsError> {
    let cert_exists = std::path::Path::new(cert_path).exists();
    let key_exists = std::path::Path::new(key_path).exists();

    match (cert_exists, key_exists) {
        (true, true) => load_existing(cert_path, key_path),
        (false, false) => bootstrap_and_write(cert_path, key_path, bootstrap_common_name),
        (true, false) => Err(CaMaterialsError::PartialState {
            existing: cert_path.to_string(),
            missing: key_path.to_string(),
        }),
        (false, true) => Err(CaMaterialsError::PartialState {
            existing: key_path.to_string(),
            missing: cert_path.to_string(),
        }),
    }
}

/// Read both files, validate them, and return the PEMs.
fn load_existing(cert_path: &str, key_path: &str) -> Result<(String, String), CaMaterialsError> {
    // Permissions check first — if the key is world-readable, we
    // refuse to load it at all. Doing this before reading the file
    // avoids the case where we read a key we shouldn't be reading
    // and then complain about it after the fact.
    check_key_permissions(key_path)?;

    let cert_pem = std::fs::read_to_string(cert_path)
        .map_err(|e| CaMaterialsError::Other(format!("read {cert_path}: {e}")))?;
    let key_pem = std::fs::read_to_string(key_path)
        .map_err(|e| CaMaterialsError::Other(format!("read {key_path}: {e}")))?;

    validate_pair(&cert_pem, &key_pem)?;

    tracing::info!(
        cert_path = %cert_path,
        key_path = %key_path,
        "loaded existing CA materials"
    );
    Ok((cert_pem, key_pem))
}

/// Generate a fresh CA, write both files, and return the PEMs.
fn bootstrap_and_write(
    cert_path: &str,
    key_path: &str,
    common_name: &str,
) -> Result<(String, String), CaMaterialsError> {
    tracing::warn!(
        cert_path = %cert_path,
        key_path = %key_path,
        "no CA materials found; bootstrapping a fresh CA root \
         (this is expected on first run; if this happens on a restart, \
         the persistent volume holding CA materials may have been wiped)"
    );

    let (cert_pem, key_pem) = bootstrap_ca(common_name)
        .map_err(|e| CaMaterialsError::Other(format!("bootstrap: {e}")))?;

    // Write cert first: if anything fails, the partial-state branch
    // of the state machine catches it on next startup. (We could
    // also write to .tmp files and rename, but the partial-state
    // detection makes that unnecessary — the operator gets a clear
    // error either way.)
    if let Some(parent) = std::path::Path::new(cert_path).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| {
            CaMaterialsError::Other(format!("create dir {}: {e}", parent.display()))
        })?;
    }
    std::fs::write(cert_path, &cert_pem)
        .map_err(|e| CaMaterialsError::Other(format!("write {cert_path}: {e}")))?;

    if let Some(parent) = std::path::Path::new(key_path).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| {
            CaMaterialsError::Other(format!("create dir {}: {e}", parent.display()))
        })?;
    }
    std::fs::write(key_path, &key_pem)
        .map_err(|e| CaMaterialsError::Other(format!("write {key_path}: {e}")))?;

    // Set 0600 on the key. On non-Unix platforms this is a no-op;
    // the permissions check on subsequent loads is also a no-op
    // there, so the behavior stays internally consistent.
    set_key_mode_0600(key_path)?;

    tracing::info!(
        cert_path = %cert_path,
        key_path = %key_path,
        "bootstrapped fresh CA materials"
    );
    Ok((cert_pem, key_pem))
}

/// On Unix, verify the key file's mode is exactly 0600.
///
/// Mode 0600 means owner-read-write, no group, no other. Anything
/// looser means another user on the host could read the key. The
/// cert issuer running as a containerized service has exactly one
/// legitimate reader — itself — so anything that grants more
/// access than that is a misconfiguration we refuse to participate
/// in.
///
/// On non-Unix platforms this function is a no-op. Windows ACLs
/// don't map cleanly to mode bits, and pretending otherwise would
/// produce false positives. If we ever support Windows production
/// deployments, this needs a real implementation.
#[cfg(unix)]
fn check_key_permissions(key_path: &str) -> Result<(), CaMaterialsError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(key_path)
        .map_err(|e| CaMaterialsError::Other(format!("stat {key_path}: {e}")))?;
    let mode = metadata.mode() & 0o777;
    if mode != 0o600 {
        return Err(CaMaterialsError::InsecureKeyPermissions {
            path: key_path.to_string(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_key_permissions(_key_path: &str) -> Result<(), CaMaterialsError> {
    Ok(())
}

/// Permission check for a provisioned CA key. Looser than
/// `check_key_permissions`' exact-0600: an operator-provisioned Secret
/// is mounted by the kubelet, and a non-root pod can only read it when
/// the file is group-readable. Group-write and any access at all for
/// others are still refused.
#[cfg(unix)]
fn check_provisioned_key_permissions(key_path: &str) -> Result<(), CaMaterialsError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(key_path)
        .map_err(|e| CaMaterialsError::Other(format!("stat {key_path}: {e}")))?;
    let mode = metadata.mode() & 0o777;
    if mode & 0o027 != 0 {
        return Err(CaMaterialsError::InsecureProvisionedKeyPermissions {
            path: key_path.to_string(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_provisioned_key_permissions(_key_path: &str) -> Result<(), CaMaterialsError> {
    Ok(())
}

/// Load CA materials an operator provisioned ahead of time (the admin
/// tool's `bootstrap-ca`). Unlike `load_or_bootstrap_ca`, there is no
/// generate branch: missing files are an error, so the service can
/// never silently mint a new root. The cert/key pair is still checked
/// for consistency, and the key for safe permissions.
pub fn load_provisioned_ca(
    cert_path: &str,
    key_path: &str,
) -> Result<(String, String), CaMaterialsError> {
    let cert_exists = std::path::Path::new(cert_path).exists();
    let key_exists = std::path::Path::new(key_path).exists();

    match (cert_exists, key_exists) {
        (true, true) => {}
        (false, false) => {
            return Err(CaMaterialsError::NotProvisioned {
                cert: cert_path.to_string(),
                key: key_path.to_string(),
            });
        }
        (true, false) => {
            return Err(CaMaterialsError::PartialState {
                existing: cert_path.to_string(),
                missing: key_path.to_string(),
            });
        }
        (false, true) => {
            return Err(CaMaterialsError::PartialState {
                existing: key_path.to_string(),
                missing: cert_path.to_string(),
            });
        }
    }

    check_provisioned_key_permissions(key_path)?;

    let cert_pem = std::fs::read_to_string(cert_path)
        .map_err(|e| CaMaterialsError::Other(format!("read {cert_path}: {e}")))?;
    let key_pem = std::fs::read_to_string(key_path)
        .map_err(|e| CaMaterialsError::Other(format!("read {key_path}: {e}")))?;

    validate_pair(&cert_pem, &key_pem)?;

    tracing::info!(
        cert_path = %cert_path,
        key_path = %key_path,
        "loaded provisioned CA materials"
    );
    Ok((cert_pem, key_pem))
}

/// On Unix, set the key file's mode to 0600.
#[cfg(unix)]
fn set_key_mode_0600(key_path: &str) -> Result<(), CaMaterialsError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(key_path)
        .map_err(|e| CaMaterialsError::Other(format!("stat {key_path}: {e}")))?
        .permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(key_path, perms)
        .map_err(|e| CaMaterialsError::Other(format!("chmod {key_path}: {e}")))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_key_mode_0600(_key_path: &str) -> Result<(), CaMaterialsError> {
    Ok(())
}

/// Validate that a cert PEM and a key PEM form a matching pair.
///
/// Compares the public key embedded in the cert to the public half
/// of the keypair. Mismatches are surfaced as
/// `CaMaterialsError::Mismatch` rather than `Other` so callers
/// (and the test suite) can distinguish "the operator has a real
/// problem" from "the file system did something weird."
///
/// We do this by re-deriving the public key from the loaded
/// keypair via rcgen and comparing the SubjectPublicKeyInfo bytes
/// to the cert's. A byte-level comparison is sufficient: if the
/// SPKI matches, the cert was issued for this key.
fn validate_pair(cert_pem: &str, key_pem: &str) -> Result<(), CaMaterialsError> {
    // Parse cert → extract SPKI
    let (_, pem_block) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes())
        .map_err(|e| CaMaterialsError::Other(format!("parse CA cert PEM: {e}")))?;

    let (_, cert) = x509_parser::parse_x509_certificate(&pem_block.contents)
        .map_err(|e| CaMaterialsError::Other(format!("parse CA cert X509: {e}")))?;

    let cert_spki = cert.public_key().raw;

    // Parse key → derive SPKI via rcgen → reparse
    let key_pair = KeyPair::from_pem(key_pem)
        .map_err(|e| CaMaterialsError::Other(format!("parse CA key: {e}")))?;

    let key_pub_pem = key_pair.public_key_pem();

    let (_, pub_pem_block) = x509_parser::pem::parse_x509_pem(key_pub_pem.as_bytes())
        .map_err(|e| CaMaterialsError::Other(format!("parse public key PEM: {e}")))?;

    let key_spki = pub_pem_block.contents;

    if cert_spki != key_spki {
        return Err(CaMaterialsError::Mismatch {
            detail: "public key mismatch".to_string(),
        });
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// cert-issuer/agent/src/ca.rs — #[cfg(test)] mod tests

#[cfg(test)]
mod tests {
    use super::*;
    use cert_issuer_common::CertType;
    use rcgen::CertificateParams as RcgenParams;
    use std::sync::OnceLock;

    fn test_ca_materials() -> &'static (String, String) {
        static CA: OnceLock<(String, String)> = OnceLock::new();
        CA.get_or_init(|| bootstrap_ca("Test CA").expect("bootstrap"))
    }

    fn make_client() -> RcgenCaClient {
        let (cert, key) = test_ca_materials();
        RcgenCaClient::new(cert, key).expect("client construction")
    }

    fn make_csr(identity: &str) -> String {
        let params = RcgenParams::new(vec![identity.to_string()]).expect("csr params");
        let key_pair = KeyPair::generate().expect("csr keypair");
        let csr = params
            .serialize_request(&key_pair)
            .expect("CSR serialization");
        csr.pem().expect("CSR PEM")
    }

    fn sample_request(identity: &str) -> CaIssueRequest {
        CaIssueRequest {
            csr_pem: make_csr(identity),
            sans: vec![identity.to_string()],
            lifetime: Duration::from_secs(3600),
            cert_type: CertType::Client,
            subject_dn: Default::default(),
            path: cert_issuer_common::record::IssuePath::TokenOnly,
            instance_binding: None,
        }
    }

    fn server_request(identity: &str) -> CaIssueRequest {
        CaIssueRequest {
            csr_pem: make_csr(identity),
            sans: vec![identity.to_string()],
            lifetime: Duration::from_secs(3600),
            cert_type: CertType::Server,
            subject_dn: Default::default(),
            path: cert_issuer_common::record::IssuePath::TokenOnly,
            instance_binding: None,
        }
    }

    #[tokio::test]
    async fn issues_a_valid_certificate() {
        let client = make_client();
        let req = sample_request("agent.polar.svc.cluster.local");
        let result = client.issue(req).await.expect("must succeed");

        assert!(result.certificate_pem.contains("BEGIN CERTIFICATE"));
        assert!(result.ca_chain_pem.contains("BEGIN CERTIFICATE"));

        let (_, pem) = x509_parser::pem::parse_x509_pem(result.certificate_pem.as_bytes())
            .expect("parseable cert");
        let (_, _cert) =
            x509_parser::parse_x509_certificate(&pem.contents).expect("parseable X.509");
    }

    #[tokio::test]
    async fn issued_cert_has_expected_san() {
        let client = make_client();
        let identity = "git-observer.polar.svc.cluster.local";
        let req = sample_request(identity);
        let result = client.issue(req).await.expect("must succeed");

        let (_, pem) =
            x509_parser::pem::parse_x509_pem(result.certificate_pem.as_bytes()).expect("pem");
        let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents).expect("x509");

        let san_ext = cert
            .extensions()
            .iter()
            .find_map(|e| match e.parsed_extension() {
                x509_parser::extensions::ParsedExtension::SubjectAlternativeName(s) => Some(s),
                _ => None,
            })
            .expect("SAN extension present");

        let dns_names: Vec<&str> = san_ext
            .general_names
            .iter()
            .filter_map(|n| match n {
                x509_parser::extensions::GeneralName::DNSName(d) => Some(*d),
                _ => None,
            })
            .collect();
        assert_eq!(dns_names, vec![identity]);
    }

    #[tokio::test]
    async fn permission_shaped_san_is_embedded_as_uri_not_dns_name() {
        let client = make_client();
        let identity = "git-observer.polar.svc.cluster.local";
        let permission_san = "p:s2s:cassini.cluster.local";
        // A topic and a group are the same kind of metadata, and land the same way.
        let topic_san = "p:sub:jira:consumer:projects";
        let group_san = "g:cassini-client";
        let req = CaIssueRequest {
            csr_pem: make_csr(identity),
            sans: vec![
                identity.to_string(),
                permission_san.to_string(),
                topic_san.to_string(),
                group_san.to_string(),
            ],
            lifetime: Duration::from_secs(3600),
            cert_type: CertType::Client,
            subject_dn: Default::default(),
            path: cert_issuer_common::record::IssuePath::TokenOnly,
            instance_binding: None,
        };
        let result = client.issue(req).await.expect("must succeed");

        let (_, pem) =
            x509_parser::pem::parse_x509_pem(result.certificate_pem.as_bytes()).expect("pem");
        let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents).expect("x509");

        let san_ext = cert
            .extensions()
            .iter()
            .find_map(|e| match e.parsed_extension() {
                x509_parser::extensions::ParsedExtension::SubjectAlternativeName(s) => Some(s),
                _ => None,
            })
            .expect("SAN extension present");

        let dns_names: Vec<&str> = san_ext
            .general_names
            .iter()
            .filter_map(|n| match n {
                x509_parser::extensions::GeneralName::DNSName(d) => Some(*d),
                _ => None,
            })
            .collect();
        let uris: Vec<&str> = san_ext
            .general_names
            .iter()
            .filter_map(|n| match n {
                x509_parser::extensions::GeneralName::URI(u) => Some(*u),
                _ => None,
            })
            .collect();

        // The permission-shaped SAN must land as URI, not DnsName --
        // and the identity SAN must still be DnsName, proving the
        // distinction is genuinely per-entry, not all-or-nothing.
        assert_eq!(dns_names, vec![identity]);
        assert_eq!(uris, vec![permission_san, topic_san, group_san]);
    }

    #[tokio::test]
    async fn ordinary_extra_san_is_still_embedded_as_dns_name() {
        let client = make_client();
        let identity = "git-observer.polar.svc.cluster.local";
        let extra_hostname = "extra-host.cluster.local";
        let req = CaIssueRequest {
            csr_pem: make_csr(identity),
            sans: vec![identity.to_string(), extra_hostname.to_string()],
            lifetime: Duration::from_secs(3600),
            cert_type: CertType::Client,
            subject_dn: Default::default(),
            path: cert_issuer_common::record::IssuePath::TokenOnly,
            instance_binding: None,
        };
        let result = client.issue(req).await.expect("must succeed");

        let (_, pem) =
            x509_parser::pem::parse_x509_pem(result.certificate_pem.as_bytes()).expect("pem");
        let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents).expect("x509");

        let san_ext = cert
            .extensions()
            .iter()
            .find_map(|e| match e.parsed_extension() {
                x509_parser::extensions::ParsedExtension::SubjectAlternativeName(s) => Some(s),
                _ => None,
            })
            .expect("SAN extension present");

        let dns_names: Vec<&str> = san_ext
            .general_names
            .iter()
            .filter_map(|n| match n {
                x509_parser::extensions::GeneralName::DNSName(d) => Some(*d),
                _ => None,
            })
            .collect();

        // A non-permission-shaped extra SAN must keep its existing,
        // pre-change behavior -- proving this fix didn't regress the
        // ordinary case while fixing the permission one.
        assert_eq!(dns_names, vec![identity, extra_hostname]);
    }

    #[tokio::test]
    async fn malformed_san_returns_an_error_not_a_panic() {
        let client = make_client();
        let identity = "git-observer.polar.svc.cluster.local";
        // Non-ASCII -- Ia5String::try_from genuinely rejects this;
        // before this fix, the surrounding .unwrap() would have
        // panicked the whole process on input like this rather than
        // returning a real error.
        let req = CaIssueRequest {
            csr_pem: make_csr(identity),
            sans: vec![identity.to_string(), "café.example.com".to_string()],
            lifetime: Duration::from_secs(3600),
            cert_type: CertType::Client,
            subject_dn: Default::default(),
            path: cert_issuer_common::record::IssuePath::TokenOnly,
            instance_binding: None,
        };

        let result = client.issue(req).await;
        assert!(matches!(result, Err(CaError::Malformed(_))));
    }

    #[tokio::test]
    async fn client_cert_has_only_client_auth_eku() {
        let client = make_client();
        let req = sample_request("git-observer.polar.svc.cluster.local");
        let result = client.issue(req).await.expect("must succeed");

        let (_, pem) =
            x509_parser::pem::parse_x509_pem(result.certificate_pem.as_bytes()).expect("pem");
        let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents).expect("x509");

        let eku = cert
            .extensions()
            .iter()
            .find_map(|e| match e.parsed_extension() {
                x509_parser::extensions::ParsedExtension::ExtendedKeyUsage(eku) => Some(eku),
                _ => None,
            })
            .expect("EKU extension must be present");

        assert!(eku.client_auth, "client cert must have clientAuth EKU");
        assert!(!eku.server_auth, "client cert must not have serverAuth EKU");
    }

    #[tokio::test]
    async fn server_cert_has_server_auth_and_client_auth_eku() {
        // Server certs carry both EKUs because Cassini connects to
        // peers as a client as well as accepting connections as a
        // server. A cert with only serverAuth would fail client-side
        // TLS handshakes on those outbound connections.
        let client = make_client();
        let req = server_request("cassini.polar.svc.cluster.local");
        let result = client.issue(req).await.expect("must succeed");

        let (_, pem) =
            x509_parser::pem::parse_x509_pem(result.certificate_pem.as_bytes()).expect("pem");
        let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents).expect("x509");

        let eku = cert
            .extensions()
            .iter()
            .find_map(|e| match e.parsed_extension() {
                x509_parser::extensions::ParsedExtension::ExtendedKeyUsage(eku) => Some(eku),
                _ => None,
            })
            .expect("EKU extension must be present");

        assert!(eku.server_auth, "server cert must have serverAuth EKU");
        assert!(eku.client_auth, "server cert must have clientAuth EKU");
    }

    fn signing_request(identity: &str) -> CaIssueRequest {
        CaIssueRequest {
            cert_type: CertType::Signing,
            path: cert_issuer_common::record::IssuePath::Internal,
            ..sample_request(identity)
        }
    }

    fn parsed(pem: &str) -> Vec<u8> {
        x509_parser::pem::parse_x509_pem(pem.as_bytes())
            .expect("pem")
            .1
            .contents
    }

    #[tokio::test]
    async fn a_signing_cert_permits_signatures_for_documents_and_nothing_else() {
        let client = make_client();
        let issued = client
            .issue(signing_request(
                "credentials-agent.polar.serviceaccount.cluster.local",
            ))
            .await
            .expect("must succeed");
        let der = parsed(&issued.certificate_pem);
        let (_, cert) = x509_parser::parse_x509_certificate(&der).expect("x509");

        let ku = cert.key_usage().expect("one").expect("KeyUsage present");
        assert!(ku.value.digital_signature());
        assert_eq!(ku.value.flags, 1, "digitalSignature and nothing else");

        let eku = cert
            .extended_key_usage()
            .expect("one")
            .expect("EKU present: one that is absent permits every use");
        assert!(!eku.value.client_auth && !eku.value.server_auth);
        assert!(!eku.value.any && !eku.value.code_signing && !eku.value.email_protection);
        let oids: Vec<String> = eku.value.other.iter().map(|o| o.to_id_string()).collect();
        assert_eq!(oids, vec!["1.3.6.1.5.5.7.3.36".to_string()]);
    }

    /// Whether rustls takes `pem` as a client certificate, and as a server certificate for `name`, under the test CA.
    fn tls_takes(pem: &str, name: &str) -> (bool, bool) {
        use rustls::client::danger::ServerCertVerifier as _;
        use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(parsed(&test_ca_materials().0)))
            .expect("root");
        let roots = std::sync::Arc::new(roots);
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        let end_entity = CertificateDer::from(parsed(pem));
        let client = rustls::server::WebPkiClientVerifier::builder_with_provider(
            roots.clone(),
            provider.clone(),
        )
        .build()
        .expect("client verifier")
        .verify_client_cert(&end_entity, &[], UnixTime::now())
        .is_ok();
        let server = rustls::client::WebPkiServerVerifier::builder_with_provider(roots, provider)
            .build()
            .expect("server verifier")
            .verify_server_cert(
                &end_entity,
                &[],
                &ServerName::try_from(name.to_string()).expect("name"),
                &[],
                UnixTime::now(),
            )
            .is_ok();
        (client, server)
    }

    #[tokio::test]
    async fn tls_refuses_a_signing_cert_in_either_role() {
        let identity = "credentials-agent.polar.serviceaccount.cluster.local";
        let client = make_client();
        let signing = client.issue(signing_request(identity)).await.unwrap();
        assert_eq!(
            tls_takes(&signing.certificate_pem, identity),
            (false, false)
        );
        // The check itself can say yes: a server certificate is taken in both roles.
        let server = client.issue(server_request(identity)).await.unwrap();
        assert_eq!(tls_takes(&server.certificate_pem, identity), (true, true));
    }

    fn escrow_request(identity: &str) -> CaIssueRequest {
        CaIssueRequest {
            cert_type: CertType::Service,
            ..sample_request(identity)
        }
    }

    #[tokio::test]
    async fn an_escrow_cert_is_for_encrypting_to_and_nothing_else() {
        // make_csr's key is P-256: a secret is sealed to it by key agreement (HPKE), so keyAgreement is its use.
        let issued = make_client()
            .issue(escrow_request(
                "git-observer.polar.serviceaccount.cluster.local",
            ))
            .await
            .unwrap();
        let der = parsed(&issued.certificate_pem);
        let (_, cert) = x509_parser::parse_x509_certificate(&der).expect("x509");
        let ku = cert.key_usage().expect("one").expect("KeyUsage present");
        assert!(ku.value.key_agreement());
        assert_eq!(ku.value.flags, 1 << 4, "keyAgreement and nothing else");
        let eku = cert
            .extended_key_usage()
            .expect("one")
            .expect("EKU present: one that is absent permits every use");
        assert!(eku.value.email_protection);
        assert!(!eku.value.client_auth && !eku.value.server_auth && !eku.value.any);
        assert!(!eku.value.code_signing && eku.value.other.is_empty());
    }

    #[tokio::test]
    async fn an_escrow_cert_for_an_rsa_key_is_for_key_encipherment_only() {
        // A secret is sealed to an RSA key by wrapping its key (RSA-OAEP).
        let identity = "git-observer.polar.serviceaccount.cluster.local";
        let rsa = cert_issuer_common::keypair::generate_csr(
            identity,
            &cert_issuer_common::keypair::KeyAlgorithm::Rsa,
            &[],
        )
        .unwrap();
        let issued = make_client()
            .issue(CaIssueRequest {
                csr_pem: rsa.csr_pem,
                ..escrow_request(identity)
            })
            .await
            .unwrap();
        let der = parsed(&issued.certificate_pem);
        let (_, cert) = x509_parser::parse_x509_certificate(&der).expect("x509");
        let ku = cert.key_usage().expect("one").expect("KeyUsage present");
        assert_eq!(ku.value.flags, 1 << 2, "keyEncipherment and nothing else");
    }

    #[tokio::test]
    async fn an_escrow_cert_is_refused_for_a_key_nothing_is_sealed_to() {
        // An Ed25519 key only signs: no secret can be sealed to it, so it is no one's escrow key.
        let identity = "git-observer.polar.serviceaccount.cluster.local";
        let key_pair = KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        let csr_pem = RcgenParams::new(vec![identity.to_string()])
            .unwrap()
            .serialize_request(&key_pair)
            .unwrap()
            .pem()
            .unwrap();
        let refused = make_client()
            .issue(CaIssueRequest {
                csr_pem: csr_pem.clone(),
                ..escrow_request(identity)
            })
            .await;
        assert!(matches!(refused, Err(CaError::Malformed(_))), "{refused:?}");
        // The same key is still a client's.
        let client = make_client()
            .issue(CaIssueRequest {
                csr_pem,
                ..sample_request(identity)
            })
            .await;
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn tls_refuses_an_escrow_cert_in_either_role() {
        // The escrow certificate is the application layer's (whom a secret is encrypted to); the transport layer
        // (who may hold a connection) must not take it.
        let identity = "git-observer.polar.serviceaccount.cluster.local";
        let escrow = make_client().issue(escrow_request(identity)).await.unwrap();
        assert_eq!(tls_takes(&escrow.certificate_pem, identity), (false, false));
    }

    #[tokio::test]
    async fn client_cert_has_digital_signature_key_usage() {
        // KeyUsage::DigitalSignature is required alongside clientAuth
        // EKU. Some TLS stacks check both; missing it causes
        // BadCertificate on stricter verifiers.
        let client = make_client();
        let req = sample_request("git-observer.polar.svc.cluster.local");
        let result = client.issue(req).await.expect("must succeed");

        let (_, pem) =
            x509_parser::pem::parse_x509_pem(result.certificate_pem.as_bytes()).expect("pem");
        let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents).expect("x509");

        let ku = cert
            .extensions()
            .iter()
            .find_map(|e| match e.parsed_extension() {
                x509_parser::extensions::ParsedExtension::KeyUsage(ku) => Some(ku),
                _ => None,
            })
            .expect("KeyUsage extension must be present");

        assert!(
            ku.digital_signature(),
            "cert must have DigitalSignature key usage"
        );
    }

    #[tokio::test]
    async fn issued_cert_has_serial_number() {
        let client = make_client();
        let req = sample_request("agent.polar.svc.cluster.local");
        let result = client.issue(req).await.expect("must succeed");

        assert!(!result.serial_number.is_empty());
        assert_ne!(result.serial_number, "unknown");
    }

    #[tokio::test]
    async fn lifetime_is_honored() {
        let client = make_client();
        let lifetime = Duration::from_secs(900);
        let req = CaIssueRequest {
            csr_pem: make_csr("agent.polar.svc.cluster.local"),
            sans: vec!["agent.polar.svc.cluster.local".to_string()],
            lifetime,
            cert_type: CertType::Client,
            subject_dn: Default::default(),
            path: cert_issuer_common::record::IssuePath::TokenOnly,
            instance_binding: None,
        };

        let before = OffsetDateTime::now_utc();
        let result = client.issue(req).await.expect("must succeed");
        let after = OffsetDateTime::now_utc();

        let expected_min = before + lifetime;
        let expected_max = after + lifetime;
        assert!(
            result.not_after >= expected_min && result.not_after <= expected_max,
            "notAfter {} not in [{}, {}]",
            result.not_after,
            expected_min,
            expected_max,
        );
    }

    #[tokio::test]
    async fn malformed_csr_returns_malformed() {
        let client = make_client();
        let req = CaIssueRequest {
            csr_pem: "not a CSR".to_string(),
            sans: vec!["agent.polar.svc.cluster.local".to_string()],
            lifetime: Duration::from_secs(3600),
            cert_type: CertType::Client,
            subject_dn: Default::default(),
            path: cert_issuer_common::record::IssuePath::TokenOnly,
            instance_binding: None,
        };

        let err = client.issue(req).await.expect_err("must fail");
        assert!(matches!(err, CaError::Malformed(_)));
    }

    #[tokio::test]
    async fn issued_cert_is_signed_by_the_ca() {
        let client = make_client();
        let req = sample_request("agent.polar.svc.cluster.local");
        let result = client.issue(req).await.expect("must succeed");

        let (_, leaf_pem) =
            x509_parser::pem::parse_x509_pem(result.certificate_pem.as_bytes()).unwrap();
        let (_, leaf) = x509_parser::parse_x509_certificate(&leaf_pem.contents).unwrap();

        let (_, ca_pem) = x509_parser::pem::parse_x509_pem(result.ca_chain_pem.as_bytes()).unwrap();
        let (_, ca) = x509_parser::parse_x509_certificate(&ca_pem.contents).unwrap();

        assert_eq!(
            leaf.issuer().as_raw(),
            ca.subject().as_raw(),
            "leaf cert issuer DN must match CA subject DN"
        );
    }

    #[test]
    fn bootstrap_produces_valid_ca() {
        let (cert_pem, key_pem) = bootstrap_ca("Bootstrap Test").expect("bootstrap");

        let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes()).expect("ca cert pem");
        let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents).expect("ca x509");

        let bc = cert
            .extensions()
            .iter()
            .find_map(|e| match e.parsed_extension() {
                x509_parser::extensions::ParsedExtension::BasicConstraints(b) => Some(b),
                _ => None,
            })
            .expect("BasicConstraints present on CA");
        assert!(bc.ca, "is_ca must be true");

        KeyPair::from_pem(&key_pem).expect("ca key pem parses");
    }

    #[test]
    fn rejects_invalid_ca_pem() {
        let result = RcgenCaClient::new("not a cert", "not a key");
        assert!(result.is_err());
    }

    // ---- load_or_bootstrap_ca tests ------------------------------------

    use tempfile::TempDir;

    fn fresh_paths(dir: &TempDir) -> (String, String) {
        let cert = dir.path().join("ca.crt").to_string_lossy().into_owned();
        let key = dir.path().join("ca.key").to_string_lossy().into_owned();
        (cert, key)
    }

    #[test]
    fn first_run_with_neither_file_present_bootstraps() {
        let dir = TempDir::new().unwrap();
        let (cert_path, key_path) = fresh_paths(&dir);

        let (cert_pem, key_pem) = load_or_bootstrap_ca(&cert_path, &key_path, "Test CA")
            .expect("bootstrap should succeed");

        assert!(cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(key_pem.contains("BEGIN"));
        assert!(std::path::Path::new(&cert_path).exists());
        assert!(std::path::Path::new(&key_path).exists());

        validate_pair(&cert_pem, &key_pem).expect("bootstrapped pair must validate");
    }

    #[test]
    fn second_run_with_both_files_loads_existing() {
        let dir = TempDir::new().unwrap();
        let (cert_path, key_path) = fresh_paths(&dir);

        let (cert1, key1) = load_or_bootstrap_ca(&cert_path, &key_path, "Test CA").unwrap();
        let (cert2, key2) = load_or_bootstrap_ca(&cert_path, &key_path, "Test CA").unwrap();

        assert_eq!(cert1, cert2, "second load must return the same cert");
        assert_eq!(key1, key2, "second load must return the same key");
    }

    #[test]
    fn cert_present_but_key_missing_is_a_partial_state_error() {
        let dir = TempDir::new().unwrap();
        let (cert_path, key_path) = fresh_paths(&dir);
        std::fs::write(&cert_path, "anything").unwrap();

        let err = load_or_bootstrap_ca(&cert_path, &key_path, "Test CA").expect_err("must fail");
        assert!(
            matches!(err, CaMaterialsError::PartialState { .. }),
            "expected PartialState, got {err:?}",
        );
    }

    #[test]
    fn key_present_but_cert_missing_is_a_partial_state_error() {
        let dir = TempDir::new().unwrap();
        let (cert_path, key_path) = fresh_paths(&dir);
        std::fs::write(&key_path, "anything").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&key_path).unwrap().permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&key_path, perms).unwrap();
        }

        let err = load_or_bootstrap_ca(&cert_path, &key_path, "Test CA").expect_err("must fail");
        assert!(
            matches!(err, CaMaterialsError::PartialState { .. }),
            "expected PartialState, got {err:?}",
        );
    }

    #[cfg(unix)]
    #[test]
    fn key_with_loose_permissions_is_rejected() {
        let dir = TempDir::new().unwrap();
        let (cert_path, key_path) = fresh_paths(&dir);
        load_or_bootstrap_ca(&cert_path, &key_path, "Test CA").unwrap();

        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&key_path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&key_path, perms).unwrap();

        let err = load_or_bootstrap_ca(&cert_path, &key_path, "Test CA")
            .expect_err("must reject loose perms");
        match err {
            CaMaterialsError::InsecureKeyPermissions { mode, .. } => {
                assert_eq!(mode, 0o644);
            }
            other => panic!("expected InsecureKeyPermissions, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn bootstrapped_key_has_mode_0600() {
        let dir = TempDir::new().unwrap();
        let (cert_path, key_path) = fresh_paths(&dir);
        load_or_bootstrap_ca(&cert_path, &key_path, "Test CA").unwrap();

        use std::os::unix::fs::MetadataExt;
        let mode = std::fs::metadata(&key_path).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600, "bootstrapped key must be mode 0600");
    }

    #[test]
    fn mismatched_cert_and_key_is_a_mismatch_error() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let (cert_a, key_a) = fresh_paths(&dir_a);
        let (cert_b, key_b) = fresh_paths(&dir_b);
        load_or_bootstrap_ca(&cert_a, &key_a, "CA A").unwrap();
        load_or_bootstrap_ca(&cert_b, &key_b, "CA B").unwrap();

        let mismatch_dir = TempDir::new().unwrap();
        let mismatch_cert = mismatch_dir
            .path()
            .join("ca.crt")
            .to_string_lossy()
            .into_owned();
        let mismatch_key = mismatch_dir
            .path()
            .join("ca.key")
            .to_string_lossy()
            .into_owned();
        std::fs::copy(&cert_a, &mismatch_cert).unwrap();
        std::fs::copy(&key_b, &mismatch_key).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&mismatch_key).unwrap().permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&mismatch_key, perms).unwrap();
        }

        let err = load_or_bootstrap_ca(&mismatch_cert, &mismatch_key, "Test CA")
            .expect_err("must reject mismatch");
        assert!(
            matches!(err, CaMaterialsError::Mismatch { .. }),
            "expected Mismatch, got {err:?}",
        );
    }

    #[test]
    fn corrupt_cert_file_is_an_error() {
        let dir = TempDir::new().unwrap();
        let (cert_path, key_path) = fresh_paths(&dir);
        load_or_bootstrap_ca(&cert_path, &key_path, "Test CA").unwrap();
        std::fs::write(&cert_path, "garbage that is not a cert").unwrap();

        let result = load_or_bootstrap_ca(&cert_path, &key_path, "Test CA");
        assert!(result.is_err(), "corrupt cert must surface as an error");
    }

    #[test]
    fn bootstrap_creates_parent_directories() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("nested").join("subdir");
        let cert_path = nested.join("ca.crt").to_string_lossy().into_owned();
        let key_path = nested.join("ca.key").to_string_lossy().into_owned();

        load_or_bootstrap_ca(&cert_path, &key_path, "Test CA")
            .expect("must create parent dirs and bootstrap");

        assert!(std::path::Path::new(&cert_path).exists());
        assert!(std::path::Path::new(&key_path).exists());
    }
}
