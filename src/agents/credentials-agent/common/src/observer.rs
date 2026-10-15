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

//! Synchronous, observer-level rejections -- a request that never
//! reaches Cassini, never gets a `correlation_id`, and never touches
//! the trusted side at all. Distinct from
//! `issuance::IssuanceRefusalReason`, which covers a well-formed
//! request that failed a real trust decision on the consumer side.
//! This is purely the observer's own "basic well-formedness checking"
//! role, per credentials-agent-architecture.md -- it never validates a
//! credential and never makes an authorization decision.
//!
//! Malformed JSON or an unexpected field is already handled by the
//! HTTP framework's own deserialization failure (`IssuanceRequest`'s
//! `deny_unknown_fields`) and doesn't need a variant here -- this
//! covers well-formedness checks that require actual logic beyond
//! parsing, like confirming a CSR is genuinely a valid CSR.
#[derive(Debug, Clone, PartialEq, serde::Serialize, thiserror::Error)]
pub enum ObserverRejection {
    #[error("CSR is not valid PEM-encoded PKCS#10: {0}")]
    MalformedCsr(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_csr_serializes_with_its_detail() {
        let rejection = ObserverRejection::MalformedCsr("unexpected EOF".to_string());
        let json = serde_json::to_string(&rejection).unwrap();
        assert!(json.contains("unexpected EOF"));
    }

    #[test]
    fn display_message_is_human_readable() {
        let rejection = ObserverRejection::MalformedCsr("unexpected EOF".to_string());
        assert_eq!(
            rejection.to_string(),
            "CSR is not valid PEM-encoded PKCS#10: unexpected EOF"
        );
    }
}
