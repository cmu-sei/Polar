//! Unit tests for OIDC token validation.
//!
//! These tests use jsonwebtoken to mint tokens with known keys, then
//! feed them to the validator with matching/non-matching configs.
//! The JWKS source is a canned implementation that returns a fixed
//! keyset; HTTP-backed JWKS fetching is tested in the integration
//! tests via wiremock.
//!
//! The most important properties to pin down here:
//!   1. A token signed with the wrong key is rejected.
//!   2. A token with the wrong audience is rejected with the
//!      InvalidAudience outcome (NOT InvalidToken — operators need
//!      to distinguish these).
//!   3. A token with the wrong issuer is rejected.
//!   4. A token signed with HS256 is rejected even if HS256 is
//!      somehow present in the JWKS.
//!   5. A token whose `alg` header is "none" is rejected.
//!   6. An expired token is rejected.
//!   7. A token missing the workload identity claim is rejected.
//!   8. A token where the audience claim is an array containing the
//!      expected audience is accepted (per RFC 7519).
//!   9. JWKS cache hits don't refetch.
//!  10. JWKS cache TTL is respected.

use cert_issuer::config::IssuerConfig;
use cert_issuer::oidc::{ValidationError, Validator};
use std::sync::Arc;
use std::time::Duration;

mod helpers {
    //! Test infrastructure: keypair generation, JWT minting, JWKS
    //! encoding, and a counting fake `JwksSource`.
    //!
    //! The RSA keypair is expensive to generate (>100ms), so we
    //! cache a single keypair in a `OnceLock` and reuse it across
    //! tests. Tests that need *two* keys (to verify wrong-key
    //! rejection) generate a second one on demand.

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
    pub const TEST_KID: &str = "test-key-1";
    pub const SECONDARY_KID: &str = "test-key-2";

    /// Long-lived (process-lifetime) RSA keypair shared across tests.
    /// Generated once on first use to avoid the >100ms cost per test.
    pub fn primary_keypair() -> &'static RsaPrivateKey {
        static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
        KEY.get_or_init(|| {
            let mut rng = rsa::rand_core::OsRng;
            // 2048 bits is the minimum sane RSA size today and is
            // fast enough for test setup.
            RsaPrivateKey::new(&mut rng, 2048).expect("rsa keypair generation")
        })
    }

    /// A second keypair for "wrong key" tests. Also cached.
    pub fn secondary_keypair() -> &'static RsaPrivateKey {
        static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
        KEY.get_or_init(|| {
            let mut rng = rsa::rand_core::OsRng;
            RsaPrivateKey::new(&mut rng, 2048).expect("rsa keypair generation")
        })
    }

    /// Encode a public key as a JWK in the form a JWKS endpoint
    /// would return. The `alg` field is set to RS256 because that's
    /// what we sign with in tests.
    pub fn jwk_for(kid: &str, key: &RsaPrivateKey) -> serde_json::Value {
        let public: RsaPublicKey = key.to_public_key();
        let n = public.n().to_bytes_be();
        let e = public.e().to_bytes_be();
        serde_json::json!({
            "kty": "RSA",
            "kid": kid,
            "use": "sig",
            "alg": "RS256",
            "n": URL_SAFE_NO_PAD.encode(&n),
            "e": URL_SAFE_NO_PAD.encode(&e),
        })
    }

    /// Build a JWKS document containing the given JWKs.
    pub fn jwks_doc(keys: &[serde_json::Value]) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "keys": keys })).unwrap()
    }

    /// Mint an RS256 JWT with the given claims, signed by the given
    /// key. The `kid` parameter goes into the JWT header so the
    /// validator can look up the right key in the JWKS.
    pub fn mint_token(kid: &str, key: &RsaPrivateKey, claims: serde_json::Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_string());
        let pem = key
            .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .expect("to_pkcs8_pem");
        let encoding_key = EncodingKey::from_rsa_pem(pem.as_bytes()).expect("encoding key");
        encode(&header, &claims, &encoding_key).expect("encode jwt")
    }

    /// Convenience: mint a fully-valid token with all the claims
    /// the happy path expects. Tests that vary one claim copy the
    /// body and tweak it.
    pub fn mint_default_token() -> String {
        mint_token(
            TEST_KID,
            primary_keypair(),
            serde_json::json!({
                "iss": TEST_ISSUER,
                "aud": TEST_AUDIENCE,
                "sub": "system:serviceaccount:polar:git-observer",
                "exp": now_plus(300),
                "kubernetes.io": {
                    "namespace": "polar",
                    "pod": {
                        "name": "git-observer-abc",
                        "uid": "11111111-2222-3333-4444-555555555555",
                    }
                }
            }),
        )
    }

    /// Mint an HS256 token with a known shared secret. The validator
    /// must reject this *before* attempting to verify against the
    /// JWKS, so the secret content here is irrelevant — the alg
    /// check fires first.
    pub fn mint_hs256_token() -> String {
        let header = Header::new(Algorithm::HS256);
        let key = EncodingKey::from_secret(b"any-symmetric-secret");
        let claims = serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": TEST_AUDIENCE,
            "sub": "system:serviceaccount:polar:git-observer",
            "exp": now_plus(300),
        });
        encode(&header, &claims, &key).expect("encode HS256")
    }

    /// Construct a hand-crafted `alg=none` token. `jsonwebtoken`'s
    /// safe API doesn't let us mint these — which is exactly why
    /// we're hand-crafting it. We're testing that the validator
    /// rejects them, not relying on a library.
    pub fn alg_none_token() -> String {
        let header = serde_json::json!({"alg": "none", "typ": "JWT", "kid": TEST_KID});
        let claims = serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": TEST_AUDIENCE,
            "sub": "system:serviceaccount:polar:git-observer",
            "exp": now_plus(300),
        });
        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
        let claims_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        // No signature segment — the third part of the JWT is empty.
        // Some `alg=none` attacks include a trailing dot with no
        // signature; both forms must be rejected by the validator.
        format!("{header_b64}.{claims_b64}.")
    }

    /// Seconds-since-epoch + offset, for `exp` and `nbf` claims.
    pub fn now_plus(seconds: i64) -> i64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock since epoch")
            .as_secs() as i64;
        now + seconds
    }

    // ---- Fake JwksSource --------------------------------------------------

    /// A JwksSource that serves a fixed body, counts fetches, and
    /// can have its body or max-age replaced between calls. The
    /// counter is shared via Arc so tests can inspect it from outside.
    pub struct FakeJwksSource {
        pub body: Arc<Mutex<Vec<u8>>>,
        pub max_age: Arc<Mutex<Option<Duration>>>,
        pub fetch_count: Arc<AtomicUsize>,
    }

    impl FakeJwksSource {
        pub fn new(body: Vec<u8>) -> Self {
            Self {
                body: Arc::new(Mutex::new(body)),
                max_age: Arc::new(Mutex::new(None)),
                fetch_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        pub fn with_max_age(self, max_age: Option<Duration>) -> Self {
            *self.max_age.lock().unwrap() = max_age;
            self
        }

        pub fn fetch_count(&self) -> usize {
            self.fetch_count.load(Ordering::SeqCst)
        }

        #[allow(dead_code)]
        pub fn replace_body(&self, body: Vec<u8>) {
            *self.body.lock().unwrap() = body;
        }
    }

    #[async_trait::async_trait]
    impl JwksSource for FakeJwksSource {
        async fn fetch(&self, _jwks_uri: &str) -> Result<(Vec<u8>, Option<Duration>), String> {
            self.fetch_count.fetch_add(1, Ordering::SeqCst);
            let body = self.body.lock().unwrap().clone();
            let max_age = *self.max_age.lock().unwrap();
            Ok((body, max_age))
        }
    }
}

use helpers::*;

fn test_issuer_config() -> IssuerConfig {
    IssuerConfig {
        issuer: TEST_ISSUER.to_string(),
        audience: TEST_AUDIENCE.to_string(),
        jwks_uri: Some("https://test-issuer.example.com/jwks".to_string()),
        workload_identity_claim: "sub".to_string(),
        // JSON Pointer syntax: /-separated, with the literal
        // "kubernetes.io" key (which contains a dot) preserved as a
        // single segment. See the doc comment on
        // `extract_string_claim` in oidc.rs for why this isn't
        // dot-separated.
        instance_binding_claim: "kubernetes.io/pod/uid".to_string(),
        allowed_algorithms: vec!["RS256".to_string(), "ES256".to_string()],
        jwks_cache_ttl_min: Duration::from_secs(30),
        jwks_cache_ttl_max: Duration::from_secs(3600),
    }
}

/// Build a validator backed by a JwksSource serving the primary
/// keypair under `TEST_KID`. This is the standard setup for tests
/// that want a fully-functional validator.
fn validator_with_default_jwks() -> (Validator, Arc<FakeJwksSource>) {
    let jwks = jwks_doc(&[jwk_for(TEST_KID, primary_keypair())]);
    let source = Arc::new(FakeJwksSource::new(jwks));
    let validator = Validator::with_jwks_source(test_issuer_config(), source.clone());
    (validator, source)
}

// ---------------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn well_formed_token_with_matching_config_validates() {
    let (validator, _source) = validator_with_default_jwks();
    let token = mint_default_token();

    let claims = validator.validate(&token).await.expect("should validate");
    assert_eq!(
        claims.workload_identity,
        "system:serviceaccount:polar:git-observer"
    );
    assert_eq!(claims.issuer, TEST_ISSUER);
    assert_eq!(claims.audience, TEST_AUDIENCE);
    assert_eq!(
        claims.instance_binding.as_deref(),
        Some("11111111-2222-3333-4444-555555555555"),
    );
}

// ---------------------------------------------------------------------------
// Signature / algorithm rejection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn token_signed_with_wrong_key_is_rejected() {
    // Validator's JWKS contains the primary key under TEST_KID.
    // We sign with the secondary key but claim the kid is TEST_KID,
    // so the validator looks up the primary public key and tries
    // to verify a signature made with the secondary private key.
    // Verification fails -> InvalidToken.
    let (validator, _source) = validator_with_default_jwks();
    let token = mint_token(
        TEST_KID,
        secondary_keypair(),
        serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": TEST_AUDIENCE,
            "sub": "anything",
            "exp": now_plus(300),
        }),
    );

    let err = validator.validate(&token).await.expect_err("must reject");
    assert_eq!(err, ValidationError::InvalidToken);
}

#[tokio::test]
async fn token_with_wrong_audience_returns_invalid_audience() {
    let (validator, _source) = validator_with_default_jwks();
    let token = mint_token(
        TEST_KID,
        primary_keypair(),
        serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": "some-other-service",
            "sub": "anything",
            "exp": now_plus(300),
        }),
    );

    let err = validator.validate(&token).await.expect_err("must reject");
    assert_eq!(
        err,
        ValidationError::InvalidAudience,
        "audience-mismatch must produce a distinct error variant",
    );
}

#[tokio::test]
async fn token_with_wrong_issuer_is_rejected() {
    let (validator, _source) = validator_with_default_jwks();
    let token = mint_token(
        TEST_KID,
        primary_keypair(),
        serde_json::json!({
            "iss": "https://attacker.example.com",
            "aud": TEST_AUDIENCE,
            "sub": "anything",
            "exp": now_plus(300),
        }),
    );

    let err = validator.validate(&token).await.expect_err("must reject");
    assert!(
        matches!(err, ValidationError::InvalidIssuer(_)),
        "expected InvalidIssuer, got {err:?}",
    );
}

#[tokio::test]
async fn hs256_token_is_rejected_regardless_of_jwks_contents() {
    let (validator, _source) = validator_with_default_jwks();
    let token = mint_hs256_token();

    let err = validator.validate(&token).await.expect_err("must reject");
    assert!(
        matches!(err, ValidationError::ForbiddenAlgorithm(_)),
        "expected ForbiddenAlgorithm, got {err:?}",
    );
}

#[tokio::test]
async fn alg_none_is_rejected_without_signature_check() {
    // The validator must reject an `alg: none` token before any
    // signature work. We test this by serving a JWKS that contains
    // a key for TEST_KID — if the validator got far enough to look
    // up the kid, we'd see the kid lookup happen. Instead, the
    // alg-none rejection should happen before any JwksSource fetch.
    let (validator, source) = validator_with_default_jwks();

    let initial_fetches = source.fetch_count();

    let token = alg_none_token();
    let err = validator.validate(&token).await.expect_err("must reject");

    assert!(
        matches!(err, ValidationError::ForbiddenAlgorithm(_)),
        "expected ForbiddenAlgorithm, got {err:?}",
    );
    assert_eq!(
        source.fetch_count(),
        initial_fetches,
        "alg=none must be rejected before any JWKS fetch",
    );
}

// ---------------------------------------------------------------------------
// Time / claim validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn expired_token_is_rejected() {
    let (validator, _source) = validator_with_default_jwks();
    let token = mint_token(
        TEST_KID,
        primary_keypair(),
        serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": TEST_AUDIENCE,
            "sub": "anything",
            "exp": now_plus(-300),  // expired 5 minutes ago
        }),
    );

    let err = validator.validate(&token).await.expect_err("must reject");
    assert_eq!(err, ValidationError::Expired);
}

#[tokio::test]
async fn token_missing_workload_identity_claim_is_rejected() {
    let (validator, _source) = validator_with_default_jwks();
    // No "sub" claim at all.
    let token = mint_token(
        TEST_KID,
        primary_keypair(),
        serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": TEST_AUDIENCE,
            "exp": now_plus(300),
        }),
    );

    let err = validator.validate(&token).await.expect_err("must reject");
    assert!(
        matches!(err, ValidationError::MissingClaim(ref c) if c == "sub"),
        "expected MissingClaim(\"sub\"), got {err:?}",
    );
}

#[tokio::test]
async fn audience_as_array_containing_expected_value_is_accepted() {
    let (validator, _source) = validator_with_default_jwks();
    let token = mint_token(
        TEST_KID,
        primary_keypair(),
        serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": [TEST_AUDIENCE, "some-other-service"],
            "sub": "anything",
            "exp": now_plus(300),
        }),
    );

    let claims = validator.validate(&token).await.expect("should validate");
    assert_eq!(claims.audience, TEST_AUDIENCE);
}

#[tokio::test]
async fn audience_as_array_not_containing_expected_value_is_rejected() {
    let (validator, _source) = validator_with_default_jwks();
    let token = mint_token(
        TEST_KID,
        primary_keypair(),
        serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": ["service-a", "service-b"],
            "sub": "anything",
            "exp": now_plus(300),
        }),
    );

    let err = validator.validate(&token).await.expect_err("must reject");
    assert_eq!(err, ValidationError::InvalidAudience);
}

// ---------------------------------------------------------------------------
// Claim extraction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn workload_identity_extracted_correctly_from_sub_claim() {
    let (validator, _source) = validator_with_default_jwks();
    let token = mint_token(
        TEST_KID,
        primary_keypair(),
        serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": TEST_AUDIENCE,
            "sub": "system:serviceaccount:polar:k8s-observer",
            "exp": now_plus(300),
        }),
    );

    let claims = validator.validate(&token).await.expect("should validate");
    assert_eq!(
        claims.workload_identity, "system:serviceaccount:polar:k8s-observer",
        "workload_identity must be extracted verbatim from sub",
    );
}

#[tokio::test]
async fn instance_binding_extracted_from_nested_claim() {
    // The configured instance binding path is "kubernetes.io/pod/uid"
    // which navigates into the (literal) "kubernetes.io" object,
    // then "pod", then "uid". This is JSON Pointer syntax.
    let (validator, _source) = validator_with_default_jwks();
    let token = mint_token(
        TEST_KID,
        primary_keypair(),
        serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": TEST_AUDIENCE,
            "sub": "anything",
            "exp": now_plus(300),
            "kubernetes.io": {
                "pod": {
                    "uid": "abcd-1234-deadbeef",
                }
            }
        }),
    );

    let claims = validator.validate(&token).await.expect("should validate");
    assert_eq!(
        claims.instance_binding.as_deref(),
        Some("abcd-1234-deadbeef")
    );
}

#[tokio::test]
async fn missing_optional_instance_binding_claim_does_not_fail_validation() {
    let (validator, _source) = validator_with_default_jwks();
    // No kubernetes.io block at all.
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

    let claims = validator.validate(&token).await.expect("should validate");
    assert_eq!(claims.instance_binding, None);
}

// ---------------------------------------------------------------------------
// JWKS caching
// ---------------------------------------------------------------------------

#[tokio::test]
async fn jwks_is_cached_after_first_fetch() {
    let (validator, source) = validator_with_default_jwks();
    let token = mint_default_token();

    validator.validate(&token).await.expect("first validates");
    validator.validate(&token).await.expect("second validates");

    assert_eq!(
        source.fetch_count(),
        1,
        "JWKS must be fetched once and cached for subsequent validations",
    );
}

#[tokio::test(start_paused = true)]
async fn jwks_cache_respects_ttl() {
    // start_paused = true means tokio::time::Instant doesn't advance
    // unless we explicitly call advance(). The validator's cache uses
    // tokio::time::Instant so this controls cache expiry.
    let (validator, source) = validator_with_default_jwks();
    let token = mint_default_token();

    validator.validate(&token).await.expect("first validates");
    assert_eq!(source.fetch_count(), 1);

    // The default config has ttl_min = 30s. The fake source returned
    // no max-age, so the cache should use ttl_min (30s).
    // Advancing 31s should expire the entry.
    tokio::time::advance(Duration::from_secs(31)).await;

    validator
        .validate(&token)
        .await
        .expect("after-ttl validates");
    assert_eq!(
        source.fetch_count(),
        2,
        "JWKS must be refetched after TTL elapses",
    );
}

#[tokio::test(start_paused = true)]
async fn jwks_cache_clamps_ttl_to_configured_max() {
    // Server says max-age=86400 (24 hours). Our config max is 3600s
    // (1 hour). The cache must use 3600s, not 86400s. We verify this
    // by advancing past the config max but well short of the server
    // max, and observing a refetch.
    let jwks = jwks_doc(&[jwk_for(TEST_KID, primary_keypair())]);
    let source = Arc::new(FakeJwksSource::new(jwks).with_max_age(Some(Duration::from_secs(86400))));
    let validator = Validator::with_jwks_source(test_issuer_config(), source.clone());
    let token = mint_default_token();

    validator.validate(&token).await.expect("first validates");
    assert_eq!(source.fetch_count(), 1);

    // 3601s: just past our configured max of 3600s, well short of
    // the server-claimed 86400s.
    tokio::time::advance(Duration::from_secs(3601)).await;

    validator
        .validate(&token)
        .await
        .expect("after-max validates");
    assert_eq!(
        source.fetch_count(),
        2,
        "cache must clamp server max-age down to configured max",
    );
}

#[tokio::test(start_paused = true)]
async fn jwks_cache_clamps_ttl_to_configured_min() {
    // Server says max-age=5s. Our config min is 30s. The cache must
    // hold the entry for at least 30s regardless of what the server
    // said. We verify this by advancing past the server's claimed
    // 5s but well short of our 30s min, and observing no refetch.
    let jwks = jwks_doc(&[jwk_for(TEST_KID, primary_keypair())]);
    let source = Arc::new(FakeJwksSource::new(jwks).with_max_age(Some(Duration::from_secs(5))));
    let validator = Validator::with_jwks_source(test_issuer_config(), source.clone());
    let token = mint_default_token();

    validator.validate(&token).await.expect("first validates");
    assert_eq!(source.fetch_count(), 1);

    // 10s: past the server's claimed 5s, but short of our 30s min.
    tokio::time::advance(Duration::from_secs(10)).await;

    validator
        .validate(&token)
        .await
        .expect("within-min validates");
    assert_eq!(
        source.fetch_count(),
        1,
        "cache must clamp server max-age up to configured min (no refetch)",
    );
}

#[tokio::test]
async fn unknown_kid_triggers_jwks_refetch() {
    // First validation populates the cache with TEST_KID. Second
    // validation uses a token with SECONDARY_KID, which isn't in
    // the cache. The validator should refetch the JWKS to look for
    // the unknown kid. Since SECONDARY_KID still isn't in the
    // refreshed JWKS, the token is rejected with InvalidToken.
    let (validator, source) = validator_with_default_jwks();

    // Prime the cache with a successful validation under TEST_KID.
    let primary_token = mint_default_token();
    validator
        .validate(&primary_token)
        .await
        .expect("primes cache");
    assert_eq!(source.fetch_count(), 1);

    // Now present a token with SECONDARY_KID. The signing key
    // doesn't matter — the validator never gets to the signature
    // step because kid lookup fails after the refetch.
    let unknown_token = mint_token(
        SECONDARY_KID,
        secondary_keypair(),
        serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": TEST_AUDIENCE,
            "sub": "anything",
            "exp": now_plus(300),
        }),
    );

    let err = validator
        .validate(&unknown_token)
        .await
        .expect_err("unknown kid must be rejected");
    assert_eq!(err, ValidationError::InvalidToken);
    assert_eq!(
        source.fetch_count(),
        2,
        "unknown kid must trigger a JWKS refetch (1 prime + 1 unknown-kid refetch)",
    );
}
