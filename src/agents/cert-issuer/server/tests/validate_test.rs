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

//! Tests for `Handler::handle_validate` -- the `/validate` endpoint
//! added for credentials-agent's two-call issuance flow (validate
//! first to get the real identity for a grant lookup, then issue
//! separately once permitted SANs are known).
//!
//! Reuses oidc_test.rs's exact Validator construction pattern rather
//! than inventing a new one. The fake CaClient panics if ever called
//! at all -- handle_validate should never reach the CA, and this
//! proves that contract implicitly rather than just by inspection.

use cert_issuer::ca::{CaClient, CaError, CaIssueRequest, IssuedCert};
use cert_issuer::config::IssuerConfig;
use cert_issuer::handler::{Handler, ValidateHandlerBody};
use cert_issuer::oidc::Validator;
use cert_issuer_common::identity::IdentityNormalization;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

mod helpers;
use helpers::*;

fn test_issuer_config() -> IssuerConfig {
    IssuerConfig {
        issuer: TEST_ISSUER.to_string(),
        audience: vec![TEST_AUDIENCE.to_string()],
        jwks_uri: Some("https://test-issuer.example.com/jwks".to_string()),
        workload_identity_claim: "sub".to_string(),
        instance_binding_claim: "kubernetes.io/pod/uid".to_string(),
        identity_normalization: IdentityNormalization::KubernetesServiceAccount {
            cluster_domain: "cluster.local".to_string(),
        },
        identity_scope_claim: None,
        allowed_algorithms: vec!["RS256".to_string(), "ES256".to_string()],
        jwks_cache_ttl_min: Duration::from_secs(30),
        jwks_cache_ttl_max: Duration::from_secs(3600),
    }
}

struct NeverCalledCa;

#[async_trait::async_trait]
impl CaClient for NeverCalledCa {
    async fn issue(&self, _req: CaIssueRequest) -> Result<IssuedCert, CaError> {
        panic!("handle_validate must never call the CA -- no CSR is even involved yet");
    }
}

fn test_handler() -> Handler {
    let jwks = jwks_doc(&[jwk_for(TEST_KID, primary_keypair())]);
    let source = Arc::new(FakeJwksSource::new(jwks));
    let validator = Validator::with_jwks_source(vec![test_issuer_config()], source);

    Handler {
        validator: Arc::new(validator),
        ca: Arc::new(NeverCalledCa),
        default_lifetime: Duration::from_secs(3600),
        server_lifetime: Duration::from_secs(43200),
        user_lifetime: Duration::from_secs(43200),
        service_lifetime: Duration::from_secs(31_536_000),
        own_workload: Default::default(),
        floor_state: None,
        identity_lifetime_overrides: HashMap::new(),
        internal_callers: vec![],
        floor: None,
        ca_cert_pem: String::new(),
    }
}

#[tokio::test]
async fn valid_token_returns_raw_workload_identity_and_instance_binding() {
    let handler = test_handler();
    let token = mint_default_token();

    let result = handler
        .handle_validate(None, &token, cert_issuer_common::ValidateRequest::default())
        .await;

    assert_eq!(result.status, 200);
    match result.body {
        ValidateHandlerBody::Success(response) => {
            // The RAW claim value, matching what an admin's grant is
            // keyed on -- not the normalized, DNS-safe identity
            // /issue's SAN-matching uses.
            assert_eq!(
                response.workload_identity,
                "system:serviceaccount:polar:git-observer"
            );
            assert_eq!(
                response.instance_binding.as_deref(),
                Some("11111111-2222-3333-4444-555555555555")
            );
        }
        ValidateHandlerBody::Error(e) => panic!("expected success, got {e:?}"),
    }
}

#[tokio::test]
async fn invalid_token_is_rejected_with_the_correct_outcome() {
    let handler = test_handler();
    let token = mint_hs256_token();

    let result = handler
        .handle_validate(None, &token, cert_issuer_common::ValidateRequest::default())
        .await;

    assert_eq!(result.status, 401);
    match result.body {
        ValidateHandlerBody::Success(_) => panic!("expected an error"),
        ValidateHandlerBody::Error(e) => {
            assert_eq!(e.outcome, cert_issuer_common::IssueOutcome::InvalidToken);
        }
    }
}

#[tokio::test]
async fn missing_instance_binding_is_none_not_an_error() {
    let handler = test_handler();
    let token = mint_token(
        TEST_KID,
        primary_keypair(),
        serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": TEST_AUDIENCE,
            "sub": "system:serviceaccount:polar:git-observer",
            "exp": now_plus(300),
        }),
    );

    let result = handler
        .handle_validate(None, &token, cert_issuer_common::ValidateRequest::default())
        .await;

    match result.body {
        ValidateHandlerBody::Success(response) => {
            assert_eq!(response.instance_binding, None);
        }
        ValidateHandlerBody::Error(e) => panic!("expected success, got {e:?}"),
    }
}

#[tokio::test]
async fn normalized_identity_is_none_when_not_requested() {
    let handler = test_handler();
    let token = mint_default_token();

    let result = handler
        .handle_validate(None, &token, cert_issuer_common::ValidateRequest::default())
        .await;

    match result.body {
        ValidateHandlerBody::Success(response) => {
            assert_eq!(response.normalized_identity, None);
        }
        ValidateHandlerBody::Error(e) => panic!("expected success, got {e:?}"),
    }
}

#[tokio::test]
async fn normalized_identity_is_included_and_genuinely_normalized_when_requested() {
    let handler = test_handler();
    let token = mint_default_token();

    let result = handler
        .handle_validate(
            None,
            &token,
            cert_issuer_common::ValidateRequest {
                include_normalized_identity: true,
            },
        )
        .await;

    match result.body {
        ValidateHandlerBody::Success(response) => {
            let normalized = response
                .normalized_identity
                .expect("normalized_identity must be Some when requested");
            assert!(!normalized.is_empty());
            // Genuine normalization happened, not just a copy of the
            // raw claim -- KubernetesServiceAccount normalization
            // reshapes "system:serviceaccount:polar:git-observer"
            // into a DNS-safe SAN, which is necessarily a different
            // string.
            assert_ne!(normalized, response.workload_identity);
        }
        ValidateHandlerBody::Error(e) => panic!("expected success, got {e:?}"),
    }
}
