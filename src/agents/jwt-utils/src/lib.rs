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

//! Minimal JWT utilities for operations that deliberately don't
//! validate a token -- reading an unverified claim purely to make a
//! routing decision, before real validation happens elsewhere.
//!
//! This is NOT a JWT validation library and never should become one --
//! nothing here checks a signature, an issuer's trustworthiness, or
//! expiry. `cert-issuer/server/src/oidc.rs`'s own multi-issuer
//! validation logic is the only place a token's claims should ever be
//! treated as trustworthy. This crate exists specifically for the
//! narrower, different operation of peeking at a claim's raw value to
//! decide *how* to proceed, before that real validation occurs --
//! extracted from what was originally a private function inside
//! cert-issuer/server (used there to select which IssuerConfig to
//! validate against, before validation itself), since a second,
//! independent need for exactly this same operation came up in
//! credentials-agent (deciding whether a credential needs IdP token
//! introspection at all, since Kubernetes tokens have no equivalent).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Reads a claim's raw string value from a JWT's payload, without any
/// signature verification at all. The result must never be treated as
/// trustworthy -- it is exactly as reliable as whatever an
/// unauthenticated party chose to put in the token, no more. Suitable
/// only for routing/dispatch decisions made *before* real validation,
/// never for anything resembling an authorization decision.
///
/// Returns `None` if the token isn't a well-formed three-part JWT, the
/// payload isn't valid base64/JSON, or the named claim isn't present
/// as a string.
pub fn peek_claim(token: &str, claim_name: &str) -> Option<String> {
    let payload_b64 = token.split('.').nth(1)?;
    let payload_bytes = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let payload_json: serde_json::Value = serde_json::from_slice(&payload_bytes).ok()?;
    let claim = payload_json.get(claim_name)?.as_str()?;
    Some(claim.to_string())
}

/// Reads a claim's raw numeric (integer) value from a JWT's payload,
/// without any signature verification -- same trust caveat as
/// `peek_claim`. A separate function rather than a generalization of
/// `peek_claim` itself, since a JWT's standard numeric claims (`exp`,
/// `iat`, `nbf`) are never strings, and forcing one function to
/// interpret both shapes would either silently coerce (a real
/// correctness risk) or push interpretation onto every caller, most
/// of whom only ever need one shape or the other.
pub fn peek_numeric_claim(token: &str, claim_name: &str) -> Option<i64> {
    let payload_b64 = token.split('.').nth(1)?;
    let payload_bytes = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let payload_json: serde_json::Value = serde_json::from_slice(&payload_bytes).ok()?;
    payload_json.get(claim_name)?.as_i64()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_token(payload_json: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(payload_json);
        format!("{header}.{payload}.")
    }

    #[test]
    fn peeks_a_real_claim() {
        let token = make_token(r#"{"iss":"https://example.com","sub":"user123"}"#);
        assert_eq!(
            peek_claim(&token, "iss"),
            Some("https://example.com".to_string())
        );
        assert_eq!(peek_claim(&token, "sub"), Some("user123".to_string()));
    }

    #[test]
    fn missing_claim_returns_none() {
        let token = make_token(r#"{"iss":"https://example.com"}"#);
        assert_eq!(peek_claim(&token, "aud"), None);
    }

    #[test]
    fn malformed_token_returns_none_not_panic() {
        assert_eq!(peek_claim("not-a-jwt-at-all", "iss"), None);
        assert_eq!(peek_claim("", "iss"), None);
        assert_eq!(
            peek_claim("header.not-valid-base64!!!.signature", "iss"),
            None
        );
    }

    #[test]
    fn non_string_claim_returns_none() {
        let token = make_token(r#"{"iss": 12345}"#);
        assert_eq!(peek_claim(&token, "iss"), None);
    }

    #[test]
    fn peeks_a_real_numeric_claim() {
        let token = make_token(r#"{"exp":1767225600,"iss":"https://example.com"}"#);
        assert_eq!(peek_numeric_claim(&token, "exp"), Some(1767225600));
    }

    #[test]
    fn string_valued_claim_returns_none_for_numeric_peek() {
        // A string-shaped claim genuinely isn't a number -- must not
        // silently coerce or panic.
        let token = make_token(r#"{"iss":"https://example.com"}"#);
        assert_eq!(peek_numeric_claim(&token, "iss"), None);
    }

    #[test]
    fn missing_numeric_claim_returns_none() {
        let token = make_token(r#"{"iss":"https://example.com"}"#);
        assert_eq!(peek_numeric_claim(&token, "exp"), None);
    }
}
