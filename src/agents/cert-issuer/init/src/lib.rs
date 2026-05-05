//! Init container client for the cert issuer.
//!
//! Runs once at pod startup, performs the cert handshake, writes
//! cert/key/CA chain to a shared volume, and exits.
//!
//! The library is split from the binary so the orchestration logic
//! (read token, generate CSR, call cert issuer, write files) is
//! independently testable.

pub mod handshake;
pub mod keypair;
pub mod output;

/// Distinct exit codes for the init container, so operators can
/// distinguish failure modes from `kubectl describe pod` without
/// reading logs.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    Success = 0,
    /// Configuration is wrong (missing env, bad audience, etc).
    /// Operator action required; restart will not help.
    BadConfig = 10,
    /// Cert issuer is unreachable. Transient infrastructure failure;
    /// restart may help.
    CertIssuerUnreachable = 20,
    /// Cert issuer responded but rejected our token.
    /// Likely a deployment misconfiguration.
    TokenRejected = 21,
    /// Cert issuer responded but rejected our CSR.
    /// Indicates a bug in the init container or in the SAN configuration.
    CsrRejected = 22,
    /// CA was unavailable from the cert issuer's side. Transient.
    CaUnavailable = 30,
    /// Output volume is not writable. Configuration problem (volume
    /// not mounted, wrong permissions).
    OutputUnwritable = 40,
    /// Internal error in the init container itself.
    InternalError = 99,
}
