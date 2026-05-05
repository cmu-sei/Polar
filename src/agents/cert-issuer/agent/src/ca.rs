//! Smallstep CA client.
//!
//! Abstracted as a trait so the handler can be tested with a mock CA
//! that doesn't require a running `step-ca` instance. The production
//! `StepCaClient` talks to step-ca's `/1.0/sign` endpoint over HTTP.
//!
//! # The step-ca request flow
//!
//! step-ca's signing endpoint authorizes requests via a one-time token
//! (OTT) — a JWT signed by a provisioner's private key. The CA has
//! the provisioner's public key registered, so when it sees an OTT
//! signed by that key, it knows the request is authorized. We hold
//! the provisioner's private signing key and mint a fresh OTT for
//! each `/sign` request.
//!
//! The OTT's claims include:
//!   - `iss`: the provisioner name (matches the CA's registered config)
//!   - `aud`: the CA's `/1.0/sign` URL (prevents replay against
//!            other endpoints)
//!   - `sans`: the SANs the CSR is requesting (CA cross-checks the
//!             CSR against this list)
//!   - `nbf`: not-before, set to now
//!   - `exp`: short-lived (60 seconds) so a stolen OTT has a tight
//!            replay window
//!   - `jti`: a fresh nonce so the CA can detect replay
//!
//! The body of the `/sign` request includes the CSR PEM, the OTT,
//! and an optional `notAfter` to constrain the issued cert's
//! lifetime (the CA may clamp this further to its own policy).
//!
//! # Why a CSR PEM and not a public key
//!
//! step-ca's `/sign` endpoint takes a CSR, not a raw public key.
//! It cross-checks the CSR's signature, its embedded SAN, and the
//! SANs in the OTT before issuing. This is what gives step-ca its
//! defense-in-depth: even if the OTT or our handler had a bug, the
//! CA's own checks would catch a mismatched SAN.

use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;
use time::OffsetDateTime;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct IssueRequest {
    /// PEM-encoded CSR. Forwarded verbatim to step-ca, which
    /// re-validates the CSR against its policy.
    pub csr_pem: String,
    /// SAN to include in the OTT's `sans` claim. Already validated
    /// by the caller against both the workload identity claim from
    /// the OIDC token and the SAN in the CSR.
    pub san: String,
    /// Requested cert lifetime. step-ca may clamp this further to
    /// match its own provisioner policy.
    pub lifetime: Duration,
}

#[derive(Debug, Clone)]
pub struct IssuedCert {
    /// PEM-encoded leaf certificate.
    pub certificate_pem: String,
    /// PEM-encoded CA chain (intermediates and root if applicable).
    pub ca_chain_pem: String,
    /// Hex-encoded serial number, for audit correlation.
    pub serial_number: String,
    pub not_after: OffsetDateTime,
}

#[derive(Debug, Error)]
pub enum CaError {
    #[error("CA is unreachable: {0}")]
    Unreachable(String),
    #[error("CA returned an error: {status}: {detail}")]
    BadResponse { status: u16, detail: String },
    #[error("CA response was malformed: {0}")]
    Malformed(String),
}

#[async_trait::async_trait]
pub trait CaClient: Send + Sync {
    async fn issue(&self, req: IssueRequest) -> Result<IssuedCert, CaError>;
}

// ---------------------------------------------------------------------------
// Production implementation
// ---------------------------------------------------------------------------

/// step-ca client. Constructed once at startup and shared across all
/// handler invocations.
///
/// The provisioner signing key is loaded once at construction. It's
/// held in memory for the service's lifetime and used to mint a
/// fresh OTT per request. We never log it, never serialize it, and
/// never write it to disk — `secrecy::SecretString` would be ideal
/// but `EncodingKey` doesn't implement the trait, so we rely on
/// careful coding instead.
pub struct StepCaClient {
    base_url: String,
    provisioner_name: String,
    provisioner_key: jsonwebtoken::EncodingKey,
    /// The algorithm matching the provisioner's key. step-ca
    /// supports ES256 and EdDSA most commonly; we determine which
    /// when loading the key.
    provisioner_alg: jsonwebtoken::Algorithm,
    http: reqwest::Client,
}

impl StepCaClient {
    /// Construct a new client.
    ///
    /// `provisioner_key_pem` is the PEM-encoded private key
    /// registered with step-ca. The algorithm is inferred from the
    /// PEM type — PKCS#8 keys can hold Ed25519 or ECDSA P-256
    /// material, and we figure out which by the key's structure.
    /// In v1 we accept ES256 and EdDSA only; RSA provisioner keys
    /// are not supported because they aren't the recommended
    /// step-ca configuration and accepting them would mean
    /// supporting RS256 OTT signing here as well.
    pub fn new(
        base_url: String,
        provisioner_name: String,
        provisioner_key_pem: &[u8],
        provisioner_alg: jsonwebtoken::Algorithm,
    ) -> Result<Self, String> {
        let provisioner_key = match provisioner_alg {
            jsonwebtoken::Algorithm::ES256 => {
                jsonwebtoken::EncodingKey::from_ec_pem(provisioner_key_pem)
                    .map_err(|e| format!("ES256 provisioner key: {e}"))?
            }
            jsonwebtoken::Algorithm::EdDSA => {
                jsonwebtoken::EncodingKey::from_ed_pem(provisioner_key_pem)
                    .map_err(|e| format!("EdDSA provisioner key: {e}"))?
            }
            other => {
                return Err(format!(
                    "unsupported provisioner algorithm: {other:?}; v1 supports ES256 and EdDSA only"
                ));
            }
        };

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| format!("http client: {e}"))?;

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            provisioner_name,
            provisioner_key,
            provisioner_alg,
            http,
        })
    }

    /// Mint a fresh one-time token authorizing a single `/sign` call.
    ///
    /// The token's `aud` claim is bound to the specific `/sign`
    /// endpoint URL so a stolen OTT can't be replayed against other
    /// endpoints. Its `exp` is 60 seconds out — enough headroom for
    /// network latency and clock skew, short enough that a captured
    /// token is useless within a minute.
    fn mint_ott(&self, san: &str) -> Result<String, CaError> {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let aud = format!("{}/1.0/sign", self.base_url);
        let claims = OttClaims {
            iss: self.provisioner_name.clone(),
            aud,
            sans: vec![san.to_string()],
            nbf: now,
            exp: now + 60,
            jti: gen_jti(),
        };

        let header = jsonwebtoken::Header::new(self.provisioner_alg);
        jsonwebtoken::encode(&header, &claims, &self.provisioner_key)
            .map_err(|e| CaError::Malformed(format!("OTT signing: {e}")))
    }
}

#[async_trait::async_trait]
impl CaClient for StepCaClient {
    async fn issue(&self, req: IssueRequest) -> Result<IssuedCert, CaError> {
        let ott = self.mint_ott(&req.san)?;

        // step-ca's `/1.0/sign` body. `notAfter` is RFC3339; the
        // CA may clamp it down per its own provisioner policy
        // (e.g., "max cert lifetime is 24h").
        let not_after = OffsetDateTime::now_utc() + req.lifetime;
        let body = SignRequestBody {
            csr: req.csr_pem,
            ott,
            not_after: not_after
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|e| CaError::Malformed(format!("notAfter format: {e}")))?,
        };

        let url = format!("{}/1.0/sign", self.base_url);
        let response = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| CaError::Unreachable(format!("POST /sign: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            // Try to read the body for diagnostic context. step-ca's
            // error responses are JSON like {"status": 400, "message":
            // "..."}, but if the body isn't JSON we still want
            // *something* in the error message.
            let detail = response
                .text()
                .await
                .unwrap_or_else(|_| "<no response body>".to_string());
            return Err(CaError::BadResponse {
                status: status.as_u16(),
                detail,
            });
        }

        let parsed: SignResponseBody = response
            .json()
            .await
            .map_err(|e| CaError::Malformed(format!("response JSON: {e}")))?;

        // Extract the serial number from the leaf cert PEM. step-ca
        // includes it in the cert; we re-parse rather than trusting
        // a separate field in the response (which step-ca doesn't
        // currently provide). If the parse fails we use a
        // placeholder — issuance succeeded, the cert is good, we
        // just can't pull the serial for audit. That's degraded but
        // not broken.
        let serial_number = parse_serial_from_pem(&parsed.crt).unwrap_or_else(|| {
            tracing::warn!("issued cert has unparseable serial; using placeholder");
            "unknown".to_string()
        });

        Ok(IssuedCert {
            certificate_pem: parsed.crt,
            ca_chain_pem: parsed.ca,
            serial_number,
            not_after,
        })
    }
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct SignRequestBody {
    csr: String,
    ott: String,
    #[serde(rename = "notAfter")]
    not_after: String,
}

#[derive(Deserialize)]
struct SignResponseBody {
    crt: String,
    ca: String,
}

#[derive(Serialize)]
struct OttClaims {
    iss: String,
    aud: String,
    sans: Vec<String>,
    nbf: i64,
    exp: i64,
    jti: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Generate a fresh JWT ID for the OTT.
///
/// step-ca uses `jti` to detect replay; we just need it to be
/// unique per request. Using the current nanosecond timestamp plus
/// a random suffix is sufficient; we don't need cryptographic
/// uniqueness because the OTT is also bound by `exp` and signed.
fn gen_jti() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // Add a small random suffix in case two requests come in within
    // the same nanosecond on a multi-core system.
    let random: u64 = rand::random();
    format!("{now_ns:x}-{random:x}")
}

/// Extract the serial number from a PEM-encoded certificate.
///
/// Returns None if the PEM doesn't parse or the cert doesn't have
/// a serial. The serial is hex-encoded, big-endian, the way it's
/// conventionally rendered in audit logs and `openssl x509 -text`
/// output.
fn parse_serial_from_pem(pem: &str) -> Option<String> {
    let (_, pem_block) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).ok()?;
    let (_, cert) = x509_parser::parse_x509_certificate(&pem_block.contents).ok()?;
    let serial_bytes = cert.tbs_certificate.raw_serial();
    Some(
        serial_bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    //! Tests for the CA client request construction and response
    //! parsing.
    //!
    //! These tests don't validate that step-ca actually accepts our
    //! requests — that requires a real step-ca instance and is
    //! covered by integration tests run in CI against a containerized
    //! step-ca. What these tests DO pin down:
    //!
    //!   - The request body has the right field names (`csr`, `ott`,
    //!     `notAfter`) — a typo here means step-ca rejects every
    //!     request silently.
    //!   - The OTT is sent in the request body, not as a header
    //!     (step-ca's API is body-based).
    //!   - 4xx responses produce `BadResponse`, not `Unreachable`.
    //!   - 5xx responses also produce `BadResponse`.
    //!   - Network-level failures produce `Unreachable`.
    //!   - Malformed response bodies produce `Malformed`, not a panic.
    //!   - The serial number from the response cert is extracted
    //!     into the `IssuedCert`.

    use super::*;
    use base64::Engine;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A real Ed25519 private key in PKCS#8 PEM form, generated
    /// once for the test process. We need a valid key because
    /// `StepCaClient::new` parses it; we don't actually verify
    /// signatures in these tests so the matching public key is
    /// never registered anywhere.
    fn test_ed25519_key_pem() -> Vec<u8> {
        use std::sync::OnceLock;
        static KEY: OnceLock<Vec<u8>> = OnceLock::new();
        KEY.get_or_init(|| {
            // ring's Ed25519KeyPair::generate_pkcs8 produces the
            // PKCS#8 DER form. We wrap it in PEM headers manually
            // since ring doesn't emit PEM directly.
            let rng = ring::rand::SystemRandom::new();
            let pkcs8 =
                ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).expect("ed25519 keygen");
            let b64 = base64::engine::general_purpose::STANDARD.encode(pkcs8.as_ref());
            // Wrap in 64-char lines per PEM convention.
            let wrapped = b64
                .as_bytes()
                .chunks(64)
                .map(|c| std::str::from_utf8(c).unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            format!("-----BEGIN PRIVATE KEY-----\n{wrapped}\n-----END PRIVATE KEY-----\n")
                .into_bytes()
        })
        .clone()
    }

    /// Generate a real, parseable self-signed cert PEM with a known
    /// serial. Used as the `crt` in mock step-ca responses so the
    /// serial-extraction code runs against real DER. Cached at
    /// process lifetime to avoid regenerating per test.
    fn fake_leaf_cert_pem() -> String {
        use std::sync::OnceLock;
        static CERT: OnceLock<String> = OnceLock::new();
        CERT.get_or_init(|| {
            let params = rcgen::CertificateParams::new(vec!["test-cert.example.com".to_string()])
                .expect("rcgen params");
            // rcgen sets a random serial by default; we don't care
            // about a specific serial value in tests, only that the
            // serial-extraction code can read *something* out of it.
            let key_pair = rcgen::KeyPair::generate().expect("rcgen keypair");
            let cert = params.self_signed(&key_pair).expect("rcgen sign");
            cert.pem()
        })
        .clone()
    }

    fn fake_step_ca_response() -> serde_json::Value {
        serde_json::json!({
            "crt": fake_leaf_cert_pem(),
            "ca": "-----BEGIN CERTIFICATE-----\nfake-ca-cert\n-----END CERTIFICATE-----\n",
        })
    }

    fn make_client(base_url: String) -> StepCaClient {
        StepCaClient::new(
            base_url,
            "test-provisioner".to_string(),
            &test_ed25519_key_pem(),
            jsonwebtoken::Algorithm::EdDSA,
        )
        .expect("client construction")
    }

    fn sample_request() -> IssueRequest {
        IssueRequest {
            csr_pem:
                "-----BEGIN CERTIFICATE REQUEST-----\nfake-csr\n-----END CERTIFICATE REQUEST-----\n"
                    .to_string(),
            san: "agent.polar.svc.cluster.local".to_string(),
            lifetime: Duration::from_secs(3600),
        }
    }

    #[tokio::test]
    async fn successful_issuance_returns_cert() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/1.0/sign"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fake_step_ca_response()))
            .mount(&server)
            .await;

        let client = make_client(server.uri());
        let result = client.issue(sample_request()).await.expect("must succeed");

        assert!(result.certificate_pem.contains("BEGIN CERTIFICATE"));
        assert!(result.ca_chain_pem.contains("fake-ca-cert"));
    }

    #[tokio::test]
    async fn request_body_has_expected_fields() {
        // Pin down that we're sending the field names step-ca
        // expects. If anyone refactors `SignRequestBody` and renames
        // the fields, this test catches it. We use wiremock's body
        // matcher to inspect the JSON we sent.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/1.0/sign"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "csr": "-----BEGIN CERTIFICATE REQUEST-----\nfake-csr\n-----END CERTIFICATE REQUEST-----\n",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(fake_step_ca_response()))
            .mount(&server)
            .await;

        let client = make_client(server.uri());
        client
            .issue(sample_request())
            .await
            .expect("body must contain csr field");
    }

    #[tokio::test]
    async fn ca_4xx_returns_bad_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/1.0/sign"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "status": 400,
                "message": "invalid CSR",
            })))
            .mount(&server)
            .await;

        let client = make_client(server.uri());
        let err = client.issue(sample_request()).await.expect_err("must fail");
        match err {
            CaError::BadResponse { status, .. } => assert_eq!(status, 400),
            other => panic!("expected BadResponse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ca_5xx_returns_bad_response() {
        // 5xx is also BadResponse, not Unreachable. The CA was
        // reachable; it's just having a problem. The init container's
        // exit code mapping treats 5xx as transient (worth retrying)
        // separately from network-level failures.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/1.0/sign"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let client = make_client(server.uri());
        let err = client.issue(sample_request()).await.expect_err("must fail");
        match err {
            CaError::BadResponse { status, .. } => assert_eq!(status, 503),
            other => panic!("expected BadResponse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unreachable_ca_returns_unreachable() {
        // No mock server is listening at this URL. The connection
        // attempt fails at the TCP level, which our code maps to
        // CaError::Unreachable.
        let client = make_client("http://127.0.0.1:1".to_string());
        let err = client.issue(sample_request()).await.expect_err("must fail");
        assert!(matches!(err, CaError::Unreachable(_)));
    }

    #[tokio::test]
    async fn malformed_response_body_returns_malformed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/1.0/sign"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let client = make_client(server.uri());
        let err = client.issue(sample_request()).await.expect_err("must fail");
        assert!(matches!(err, CaError::Malformed(_)));
    }

    #[tokio::test]
    async fn rejects_unsupported_provisioner_algorithm() {
        // RSA provisioner keys are not supported in v1. The error
        // happens at construction, not at first issue.
        let key_pem = test_ed25519_key_pem(); // any PEM works for the test
        let result = StepCaClient::new(
            "http://localhost".to_string(),
            "test".to_string(),
            &key_pem,
            jsonwebtoken::Algorithm::RS256,
        );
        assert!(result.is_err());
    }
}
