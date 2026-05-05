//! Integration tests for the issuance handler.
//!
//! These tests exercise the full OIDC -> CSR -> CA flow using:
//!   - A canned `JwksSource` returning a fixed keyset
//!   - An in-memory mock CA implementing the CaClient trait
//!
//! The point of these tests, beyond what the unit tests cover, is
//! to pin down the *order of operations* in the handler —
//! specifically that the CA is never called when OIDC validation
//! fails or when the CSR's SAN doesn't match the token's identity.
//!
//! The token-minting and CSR-generation helpers below mirror the
//! ones in `oidc_test.rs`. They're duplicated rather than shared
//! to keep the OIDC tests independently maintainable; the cost of
//! a few hundred lines of test setup is far lower than the risk
//! of a shared-helpers refactor breaking working tests.

use cert_issuer::ca::{CaClient, CaError, IssueRequest as CaIssueRequest, IssuedCert};
use cert_issuer::config::IssuerConfig;
use cert_issuer::handler::{Handler, HandlerBody};
use cert_issuer::oidc::{JwksSource, Validator};
use cert_issuer_common::{IssueOutcome, IssueRequest};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

mod helpers {
    //! Test infrastructure shared across all handler tests in this
    //! file. Mirrors the OIDC test helpers but trimmed to what the
    //! handler tests actually need.

    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use cert_issuer::oidc::JwksSource;
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use rsa::pkcs8::EncodePrivateKey;
    use rsa::traits::PublicKeyParts;
    use rsa::{RsaPrivateKey, RsaPublicKey};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;

    pub const TEST_ISSUER: &str = "https://test-issuer.example.com";
    pub const TEST_AUDIENCE: &str = "polar-cert-issuer";
    pub const TEST_KID: &str = "handler-test-key";

    /// Long-lived RSA keypair shared across all handler tests.
    /// Cached because RSA generation costs ~100ms; running 20+
    /// tests would otherwise add 2+ seconds of test-suite startup.
    pub fn keypair() -> &'static RsaPrivateKey {
        static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
        KEY.get_or_init(|| {
            let mut rng = rsa::rand_core::OsRng;
            RsaPrivateKey::new(&mut rng, 2048).expect("rsa keypair generation")
        })
    }

    /// Build the JWKS document the validator will fetch.
    pub fn jwks_doc() -> Vec<u8> {
        let key = keypair();
        let public: RsaPublicKey = key.to_public_key();
        let n = public.n().to_bytes_be();
        let e = public.e().to_bytes_be();
        let jwk = serde_json::json!({
            "kty": "RSA",
            "kid": TEST_KID,
            "use": "sig",
            "alg": "RS256",
            "n": URL_SAFE_NO_PAD.encode(&n),
            "e": URL_SAFE_NO_PAD.encode(&e),
        });
        serde_json::to_vec(&serde_json::json!({ "keys": [jwk] })).unwrap()
    }

    /// Mint an RS256 JWT signed by the shared keypair. Tests vary
    /// the claims via the `claims` argument.
    pub fn mint_token(claims: serde_json::Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(TEST_KID.to_string());
        let pem = keypair()
            .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .expect("to_pkcs8_pem");
        let encoding_key = EncodingKey::from_rsa_pem(pem.as_bytes()).expect("encoding key");
        encode(&header, &claims, &encoding_key).expect("encode jwt")
    }

    /// Mint a fully-valid token for the given workload identity.
    pub fn mint_valid_token(workload_identity: &str) -> String {
        mint_token(serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": TEST_AUDIENCE,
            "sub": workload_identity,
            "exp": now_plus(300),
        }))
    }

    /// Mint a token claiming a custom audience. Used to verify the
    /// audience mismatch path.
    pub fn mint_token_with_audience(audience: &str) -> String {
        mint_token(serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": audience,
            "sub": "system:serviceaccount:polar:git-observer",
            "exp": now_plus(300),
        }))
    }

    /// Mint a token whose `exp` is in the past.
    pub fn mint_expired_token() -> String {
        mint_token(serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": TEST_AUDIENCE,
            "sub": "system:serviceaccount:polar:git-observer",
            "exp": now_plus(-300),
        }))
    }

    /// Generate a CSR with the given identity as its single SAN.
    /// Uses rcgen to produce a real, parseable CSR — the cert
    /// issuer's parser expects PKCS#10 with a SAN extension.
    pub fn generate_csr_for_identity(identity: &str) -> String {
        let params =
            rcgen::CertificateParams::new(vec![identity.to_string()]).expect("rcgen params");
        let key_pair = rcgen::KeyPair::generate().expect("rcgen keypair");
        let csr = params
            .serialize_request(&key_pair)
            .expect("CSR serialization");
        csr.pem().expect("PEM encoding")
    }

    pub fn now_plus(seconds: i64) -> i64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock since epoch")
            .as_secs() as i64;
        now + seconds
    }

    /// Test config matching what the validator expects. Issuer and
    /// audience are constants (TEST_ISSUER, TEST_AUDIENCE) and tokens
    /// must claim them to validate.
    pub fn test_issuer_config() -> cert_issuer::config::IssuerConfig {
        cert_issuer::config::IssuerConfig {
            issuer: TEST_ISSUER.to_string(),
            audience: TEST_AUDIENCE.to_string(),
            jwks_uri: Some("https://test-issuer.example.com/jwks".to_string()),
            workload_identity_claim: "sub".to_string(),
            instance_binding_claim: "kubernetes.io/pod/uid".to_string(),
            allowed_algorithms: vec!["RS256".to_string(), "ES256".to_string()],
            jwks_cache_ttl_min: Duration::from_secs(30),
            jwks_cache_ttl_max: Duration::from_secs(3600),
        }
    }

    // ---- Fake JwksSource --------------------------------------------------

    pub struct FakeJwksSource {
        pub body: Mutex<Vec<u8>>,
        pub fetch_count: AtomicUsize,
    }

    impl FakeJwksSource {
        pub fn new(body: Vec<u8>) -> Self {
            Self {
                body: Mutex::new(body),
                fetch_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl JwksSource for FakeJwksSource {
        async fn fetch(&self, _jwks_uri: &str) -> Result<(Vec<u8>, Option<Duration>), String> {
            self.fetch_count.fetch_add(1, Ordering::SeqCst);
            Ok((self.body.lock().unwrap().clone(), None))
        }
    }
}

use helpers::*;

/// Mock CA that records every call and returns a configurable
/// response. Tests inspect `call_count()` to verify the handler's
/// order of operations (the CA must not be called when validation
/// fails upstream).
struct MockCa {
    calls: AtomicUsize,
    response: Arc<dyn Fn() -> Result<IssuedCert, CaError> + Send + Sync>,
}

impl MockCa {
    fn always_succeed() -> Self {
        let response: Arc<dyn Fn() -> Result<IssuedCert, CaError> + Send + Sync> = Arc::new(|| {
            Ok(IssuedCert {
                certificate_pem: "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
                    .to_string(),
                ca_chain_pem: "-----BEGIN CERTIFICATE-----\nfake-ca\n-----END CERTIFICATE-----\n"
                    .to_string(),
                serial_number: "0123456789abcdef".to_string(),
                not_after: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
            })
        });
        Self {
            calls: AtomicUsize::new(0),
            response,
        }
    }

    fn always_fail() -> Self {
        let response: Arc<dyn Fn() -> Result<IssuedCert, CaError> + Send + Sync> =
            Arc::new(|| Err(CaError::Unreachable("mock CA is down".to_string())));
        Self {
            calls: AtomicUsize::new(0),
            response,
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl CaClient for MockCa {
    async fn issue(&self, _req: CaIssueRequest) -> Result<IssuedCert, CaError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        (self.response)()
    }
}

/// Build a Handler with a mocked CA and a real OIDC Validator
/// backed by a canned JWKS. The validator's JWKS source returns the
/// public key matching the keypair tests sign with, so any token
/// minted by `mint_token`/`mint_valid_token` will validate.
fn handler_with_mock_ca(mock_ca: Arc<MockCa>) -> Handler {
    let jwks_source = Arc::new(FakeJwksSource::new(jwks_doc()));
    let validator = Validator::with_jwks_source(test_issuer_config(), jwks_source);
    Handler {
        validator: Arc::new(validator),
        ca: mock_ca,
        default_lifetime: Duration::from_secs(3600),
    }
}

// ---------------------------------------------------------------------------
// End-to-end success path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn happy_path_returns_200_with_certificate() {
    let ca = Arc::new(MockCa::always_succeed());
    let handler = handler_with_mock_ca(ca.clone());

    let token = mint_valid_token("system:serviceaccount:polar:git-observer");
    let csr = generate_csr_for_identity("system:serviceaccount:polar:git-observer");

    let resp = handler.handle(&token, IssueRequest { csr_pem: csr }).await;

    assert_eq!(resp.status, 200);
    match resp.body {
        HandlerBody::Success(s) => {
            assert!(!s.certificate_pem.is_empty());
            assert!(!s.ca_chain_pem.is_empty());
            assert!(!s.session_id.is_empty());
        }
        _ => panic!("expected Success body"),
    }
    assert_eq!(
        ca.call_count(),
        1,
        "CA should be called exactly once on success"
    );
}

// ---------------------------------------------------------------------------
// Order-of-operations: CA is not called when validation fails upstream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ca_is_not_called_when_token_is_invalid() {
    let ca = Arc::new(MockCa::always_succeed());
    let handler = handler_with_mock_ca(ca.clone());

    let resp = handler
        .handle(
            "not-a-jwt",
            IssueRequest {
                csr_pem: generate_csr_for_identity("anything"),
            },
        )
        .await;

    assert_eq!(resp.status, 401);
    assert_eq!(
        ca.call_count(),
        0,
        "CA must not be called when token is invalid"
    );
}

#[tokio::test]
async fn ca_is_not_called_when_audience_is_wrong() {
    let ca = Arc::new(MockCa::always_succeed());
    let handler = handler_with_mock_ca(ca.clone());

    let token = mint_token_with_audience("some-other-service");
    let csr = generate_csr_for_identity("system:serviceaccount:polar:git-observer");

    let resp = handler.handle(&token, IssueRequest { csr_pem: csr }).await;

    assert_eq!(resp.status, 401);
    match resp.body {
        HandlerBody::Error(e) => assert_eq!(e.outcome, IssueOutcome::InvalidAudience),
        _ => panic!("expected Error body"),
    }
    assert_eq!(
        ca.call_count(),
        0,
        "CA must not be called on audience mismatch"
    );
}

#[tokio::test]
async fn ca_is_not_called_when_csr_identity_does_not_match_token_identity() {
    // The token authorizes "git-observer" but the CSR asks for
    // "k8s-observer". This is the impersonation-prevention check
    // and it must happen *before* the CA is called.
    let ca = Arc::new(MockCa::always_succeed());
    let handler = handler_with_mock_ca(ca.clone());

    let token = mint_valid_token("system:serviceaccount:polar:git-observer");
    let csr = generate_csr_for_identity("system:serviceaccount:polar:k8s-observer");

    let resp = handler.handle(&token, IssueRequest { csr_pem: csr }).await;

    assert_eq!(resp.status, 403);
    match resp.body {
        HandlerBody::Error(e) => assert_eq!(e.outcome, IssueOutcome::IdentityMismatch),
        _ => panic!("expected Error body"),
    }
    assert_eq!(
        ca.call_count(),
        0,
        "CA must not be called on identity mismatch"
    );
}

#[tokio::test]
async fn ca_is_not_called_when_csr_is_malformed() {
    let ca = Arc::new(MockCa::always_succeed());
    let handler = handler_with_mock_ca(ca.clone());

    let token = mint_valid_token("system:serviceaccount:polar:git-observer");

    let resp = handler
        .handle(
            &token,
            IssueRequest {
                csr_pem: "not a CSR".to_string(),
            },
        )
        .await;

    assert_eq!(resp.status, 400);
    match resp.body {
        HandlerBody::Error(e) => assert_eq!(e.outcome, IssueOutcome::InvalidCsr),
        _ => panic!("expected Error body"),
    }
    assert_eq!(ca.call_count(), 0, "CA must not be called on malformed CSR");
}

#[tokio::test]
async fn ca_failure_returns_503() {
    // If everything validates but the CA itself is down, return
    // 503 so the init container knows it's a transient
    // infrastructure problem rather than a deployment
    // misconfiguration.
    let ca = Arc::new(MockCa::always_fail());
    let handler = handler_with_mock_ca(ca.clone());

    let token = mint_valid_token("system:serviceaccount:polar:git-observer");
    let csr = generate_csr_for_identity("system:serviceaccount:polar:git-observer");

    let resp = handler.handle(&token, IssueRequest { csr_pem: csr }).await;

    assert_eq!(resp.status, 503);
    match resp.body {
        HandlerBody::Error(e) => assert_eq!(e.outcome, IssueOutcome::CaUnavailable),
        _ => panic!("expected Error body"),
    }
    assert_eq!(ca.call_count(), 1, "CA was called and failed");
}

#[tokio::test]
async fn expired_token_is_rejected_with_401() {
    let ca = Arc::new(MockCa::always_succeed());
    let handler = handler_with_mock_ca(ca.clone());

    let token = mint_expired_token();
    let csr = generate_csr_for_identity("system:serviceaccount:polar:git-observer");

    let resp = handler.handle(&token, IssueRequest { csr_pem: csr }).await;

    assert_eq!(resp.status, 401);
    match resp.body {
        HandlerBody::Error(e) => assert_eq!(e.outcome, IssueOutcome::InvalidToken),
        _ => panic!("expected Error body"),
    }
    assert_eq!(ca.call_count(), 0);
}

#[tokio::test]
async fn session_id_is_unique_per_request() {
    // Session IDs go into telemetry and into cert metadata for
    // audit correlation. They must be unique per request — not
    // just per workload, not just per pod.
    let ca = Arc::new(MockCa::always_succeed());
    let handler = handler_with_mock_ca(ca.clone());

    let token = mint_valid_token("system:serviceaccount:polar:git-observer");
    let csr = generate_csr_for_identity("system:serviceaccount:polar:git-observer");

    let resp1 = handler
        .handle(
            &token,
            IssueRequest {
                csr_pem: csr.clone(),
            },
        )
        .await;
    // The CSR's keypair is freshly generated each call, so the two
    // requests use different keys but the same identity. We use the
    // same csr string here (it's the second call's CSR that matters
    // less — we're testing session_id uniqueness, not CSR uniqueness).
    let resp2 = handler.handle(&token, IssueRequest { csr_pem: csr }).await;

    let id1 = match resp1.body {
        HandlerBody::Success(s) => s.session_id,
        _ => panic!("expected Success"),
    };
    let id2 = match resp2.body {
        HandlerBody::Success(s) => s.session_id,
        _ => panic!("expected Success"),
    };
    assert_ne!(id1, id2, "session IDs must be unique per request");
}
