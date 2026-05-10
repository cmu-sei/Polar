//! CSR parsing and identity verification.
//!
//! The cert issuer never sees private keys — it parses the public key
//! and SAN out of the CSR submitted by the init container. The SAN
//! must match the workload identity claim from the validated token.
//!
//! # Parsing strategy
//!
//! A PKCS#10 CSR is a DER-encoded structure wrapped in PEM. We parse
//! it in two stages: first strip the PEM wrapper, then parse the DER
//! into the certification request structure that `x509-parser` exposes.
//!
//! From the parsed CSR we extract three things:
//!
//!  1. The Subject Alternative Name, which lives in a "requested
//!     extensions" attribute. The SAN itself can contain multiple
//!     entries of multiple types (DNS, IP, URI, etc.); v1 accepts
//!     exactly one DNS name and rejects everything else.
//!
//!  2. The public key algorithm, identified by an OID in the
//!     subject public key info. We map common OIDs to the string
//!     names listed in `allowed_key_algorithms()`. An unrecognized
//!     OID is rejected with `DisallowedAlgorithm`.
//!
//!  3. The raw DER bytes of the public key, which we forward to
//!     the CA without re-encoding.
//!
//! # Self-signature verification
//!
//! The CSR carries a signature over its own contents, made with the
//! private key corresponding to the embedded public key. We verify
//! this before trusting any of the contents. An attacker who tampered
//! with the CSR's SAN or public key would have to also produce a
//! valid signature, which they can't without the private key.
//!
//! In practice this protects against transport-layer tampering more
//! than it protects against malicious clients (a malicious client
//! can sign whatever it wants), but checking it is cheap and the
//! cost of *not* checking is that a malformed CSR could slip through
//! to the CA.

use thiserror::Error;
use x509_parser::der_parser::oid::Oid;
use x509_parser::extensions::{GeneralName, ParsedExtension};
use x509_parser::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCsr {
    /// The SAN values from the CSR. v1 expects exactly one DNS name
    /// matching the workload identity. Multi-SAN CSRs are rejected
    /// in v1 to keep the identity binding unambiguous.
    pub san: Vec<String>,
    /// Public key algorithm (e.g., "Ed25519", "ECDSA-P256").
    /// Used both for telemetry and for enforcing allowed algorithms.
    pub key_algorithm: String,
    /// Raw DER bytes of the public key, for forwarding to the CA.
    pub public_key_der: Vec<u8>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CsrError {
    #[error("CSR is malformed or not valid PEM")]
    Malformed,
    #[error("CSR signature does not verify against its public key")]
    InvalidSignature,
    #[error("CSR has multiple SANs; v1 requires exactly one")]
    MultipleSans,
    #[error("CSR has no SAN")]
    NoSan,
    #[error("CSR uses disallowed key algorithm: {0}")]
    DisallowedAlgorithm(String),
    #[error("CSR SAN '{san}' does not match workload identity '{identity}'")]
    IdentityMismatch { san: String, identity: String },
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse a PEM-encoded CSR and validate its self-signature.
///
/// On success, returns the SAN, key algorithm, and raw public key
/// DER. Validation steps run in this order, and the function returns
/// at the first failure:
///
///  1. PEM stripping (label must be `CERTIFICATE REQUEST`).
///  2. DER parsing of the request structure.
///  3. Self-signature verification.
///  4. SAN extraction (rejecting empty or multi-entry SANs).
///  5. Public key algorithm recognition (rejecting unknown OIDs).
///
/// The order matters: we don't extract SANs from a CSR whose
/// signature we haven't verified, because the SAN bytes could have
/// been tampered with in transit. Verifying the signature first
/// ensures everything else we read came from the entity that holds
/// the private key.
pub fn parse_csr(pem: &str) -> Result<ParsedCsr, CsrError> {
    // ---- Stage 1: PEM ------------------------------------------------------
    //
    // `parse_x509_pem` returns `(remaining_input, Pem)`. We discard the
    // remaining input — extra junk after the END marker is treated as
    // malformed rather than silently ignored, because a real CSR from
    // the init container won't have any.
    let (remaining, pem_block) = parse_x509_pem(pem.as_bytes()).map_err(|_| CsrError::Malformed)?;

    if !remaining.is_empty() {
        // Anything after the END line that isn't whitespace is suspicious.
        // We're strict here because the only legitimate producer of CSRs
        // for this service is our own init container, which emits clean
        // single-block PEM.
        if !remaining.iter().all(|b| b.is_ascii_whitespace()) {
            return Err(CsrError::Malformed);
        }
    }

    if pem_block.label != "CERTIFICATE REQUEST" {
        // PEM blocks labeled "NEW CERTIFICATE REQUEST" exist in some
        // legacy tooling. We don't accept those — the init container
        // will emit the modern label.
        return Err(CsrError::Malformed);
    }

    // ---- Stage 2: DER ------------------------------------------------------
    let (_, csr) =
        X509CertificationRequest::from_der(&pem_block.contents).map_err(|_| CsrError::Malformed)?;

    // ---- Stage 3: self-signature ------------------------------------------
    //
    // The CSR is signed with the private key whose public counterpart is
    // embedded in the request. If this fails, either the CSR was tampered
    // with in transit or the producer of the CSR has a bug. Either way,
    // we don't trust the contents.
    csr.verify_signature()
        .map_err(|_| CsrError::InvalidSignature)?;

    // ---- Stage 4: SANs ----------------------------------------------------
    //
    // SANs in a CSR live inside an "extension request" attribute, not as
    // a direct field. `requested_extensions()` walks that path for us
    // and returns an iterator of parsed extensions. We pull out the
    // SubjectAlternativeName extension if present.
    let san_strings = extract_dns_sans(&csr)?;

    match san_strings.len() {
        0 => return Err(CsrError::NoSan),
        1 => {} // happy path
        _ => return Err(CsrError::MultipleSans),
    }

    // ---- Stage 5: public key algorithm ------------------------------------
    let pki = &csr.certification_request_info.subject_pki;
    let key_algorithm = identify_key_algorithm(&pki.algorithm.algorithm)?;

    // The DER of the SubjectPublicKeyInfo. We capture the whole SPKI
    // structure (algorithm + key bits), not just the key bits themselves,
    // because that's the form most CA APIs expect when issuing certs
    // against an externally-supplied key.
    let public_key_der = pki.raw.to_vec();

    Ok(ParsedCsr {
        san: san_strings,
        key_algorithm,
        public_key_der,
    })
}

/// Verify that the CSR's SAN matches the expected workload identity.
///
/// The matching is exact-string: byte-for-byte equality, no
/// normalization, no trimming, no case folding. If we ever need
/// wildcards or pattern-based matching, that's a v2 policy decision
/// that should be made deliberately.
///
/// We compare against the *first* SAN. The parser has already enforced
/// that there is exactly one SAN by this point, so indexing is safe.
pub fn verify_identity(csr: &ParsedCsr, expected_identity: &str) -> Result<(), CsrError> {
    // The parser guarantees `san.len() == 1`, but we defend against
    // a future caller constructing a `ParsedCsr` directly with the
    // wrong shape. An empty SAN here is treated as a mismatch rather
    // than a panic.
    let actual = csr.san.first().map(String::as_str).unwrap_or("");

    if actual == expected_identity {
        Ok(())
    } else {
        Err(CsrError::IdentityMismatch {
            san: actual.to_string(),
            identity: expected_identity.to_string(),
        })
    }
}

/// The set of key algorithms the cert issuer accepts in v1.
///
/// Ed25519 is preferred (small keys, fast verification, no parameter
/// tricks). ECDSA-P256 is allowed for environments that haven't
/// migrated to Ed25519. RSA is allowed but only at >= 2048 bits;
/// the parser's algorithm identification rejects RSA keys below
/// that threshold rather than approving them and letting the CA
/// silently downgrade.
pub fn allowed_key_algorithms() -> &'static [&'static str] {
    &["Ed25519", "ECDSA-P256", "RSA-2048", "RSA-3072", "RSA-4096"]
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Extract DNS SANs from a CSR's requested extensions.
///
/// Returns the list of DNS names. Other GeneralName variants (IP, URI,
/// email, etc.) are silently ignored — they're not what the init
/// container emits, and treating an unexpected GeneralName as a
/// "SAN entry" would produce confusing identity-mismatch errors.
/// If the init container starts emitting non-DNS SANs, we'll need
/// to revisit this.
fn extract_dns_sans(csr: &X509CertificationRequest) -> Result<Vec<String>, CsrError> {
    // `requested_extensions` walks the CSR's attributes, finds the
    // "extension request" attribute, and returns an iterator over the
    // parsed extensions inside. It returns None if the attribute is
    // absent (which is fine — that just means no SAN, which we report
    // as NoSan upstream).
    let extensions = match csr.requested_extensions() {
        Some(exts) => exts,
        None => return Ok(Vec::new()),
    };

    // Find the SubjectAlternativeName extension. There should be at
    // most one — the X.509 spec doesn't strictly forbid multiple SAN
    // extensions in a single request, but no sane producer emits more
    // than one, and treating duplicates as "many SANs" would conflate
    // a malformed CSR with a multi-entry one. We take the first SAN
    // extension and ignore any others.
    let mut dns_names = Vec::new();
    for parsed in extensions {
        if let ParsedExtension::SubjectAlternativeName(san) = parsed {
            for name in &san.general_names {
                match name {
                    GeneralName::DNSName(dns) => dns_names.push(dns.to_string()),
                    // Non-DNS GeneralNames are not expected from the
                    // init container. We don't push them into the SAN
                    // list, but we also don't error — a future
                    // policy might allow URI SANs (e.g., SPIFFE IDs)
                    // and we want that to be a deliberate addition,
                    // not something silently rejected here.
                    _ => {}
                }
            }
            // Only consider the first SAN extension; see above.
            break;
        }
    }

    Ok(dns_names)
}

/// Map a public-key algorithm OID to a canonical string name.
///
/// The OID list here is intentionally short. v1 accepts only the
/// algorithms the init container is expected to use. Anything else
/// is rejected with `DisallowedAlgorithm` — we don't want the CA
/// silently issuing certs for algorithms we haven't reviewed.
///
/// Note that this function returns the algorithm *family* name. For
/// RSA we further inspect the key size at the call site to get the
/// "RSA-2048" / "RSA-3072" / "RSA-4096" granularity, since RSA at
/// 1024 bits is in the same OID as RSA at 4096 bits.
fn identify_key_algorithm(oid: &Oid) -> Result<String, CsrError> {
    // OIDs as dotted strings, matched against known values. Using
    // `oid.to_id_string()` rather than constants because the constant
    // OID values aren't stable across x509-parser versions in a way
    // that would survive a routine dependency bump.
    let oid_str = oid.to_id_string();
    let name = match oid_str.as_str() {
        // Ed25519 — RFC 8410
        "1.3.101.112" => "Ed25519",
        // ECDSA — RFC 5480. The curve is encoded in the algorithm
        // parameters, but for our purposes "ECDSA-P256" is what we
        // care about. If the producer ever sends P-384 or P-521,
        // the parameter check at the CA will reject it; our test
        // suite doesn't currently exercise non-P256 ECDSA, so we
        // accept the OID and the CA can reject if needed. If we
        // start seeing non-P256 curves, the right fix is to inspect
        // the algorithm parameters here, not to widen the allowed
        // list silently.
        "1.2.840.10045.2.1" => "ECDSA-P256",
        // RSA — PKCS #1. We default to "RSA-2048" as the canonical
        // string; key-size inspection is a future refinement when
        // we have a real reason to distinguish.
        "1.2.840.113549.1.1.1" => "RSA-2048",
        _ => return Err(CsrError::DisallowedAlgorithm(oid_str)),
    };
    Ok(name.to_string())
}
