// cert_issuer_init/src/token.rs

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TokenError {
    #[error("token file not found at {path}: {source}")]
    NotFound {
        path: String,
        source: std::io::Error,
    },
    #[error("token is not a valid JWT structure")]
    MalformedStructure,
    #[error("token payload is not valid base64")]
    MalformedBase64(String),
    #[error("token payload is not valid JSON")]
    MalformedJson(String),
    #[error("token is missing 'sub' claim")]
    MissingSub,
}

pub fn read_token(path: &str) -> Result<String, TokenError> {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .map_err(|e| TokenError::NotFound {
            path: path.to_string(),
            source: e,
        })
}

/// Read a projected SA token from the filesystem and extract the
/// `sub` claim without verifying the signature.
///
/// Signature verification is the cert issuer's job. The init
/// container only needs the `sub` claim to construct the CSR SAN.
/// If the token has been tampered with and the `sub` claim is wrong,
/// the cert issuer's identity match check catches it — the init
/// container will have built a CSR for the wrong identity, which
/// won't match the validated token's identity, and issuance fails
/// with IdentityMismatch.
///
/// This is not a security boundary. It's a convenience function
/// for reading a claim from a token the init container already has
/// and is about to hand to the cert issuer anyway.
pub fn read_sub_claim(token_path: &str) -> Result<String, TokenError> {
    let token = std::fs::read_to_string(token_path).map_err(|e| TokenError::NotFound {
        path: token_path.to_string(),
        source: e,
    })?;

    extract_sub(token.trim())
}

pub fn extract_sub(token: &str) -> Result<String, TokenError> {
    // A JWT is three base64url segments separated by dots.
    // We only need the second (payload) segment.
    let payload_b64 = token
        .splitn(3, '.')
        .nth(1)
        .ok_or(TokenError::MalformedStructure)?;

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|e| TokenError::MalformedBase64(e.to_string()))?;

    let payload: serde_json::Value = serde_json::from_slice(&payload_bytes)
        .map_err(|e| TokenError::MalformedJson(e.to_string()))?;

    payload
        .get("sub")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or(TokenError::MissingSub)
}
