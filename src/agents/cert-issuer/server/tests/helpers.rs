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

// In helpers mod, alongside primary_keypair()
pub fn ensure_crypto_provider() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        if let Err(e) = jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER.install_default() {
            println!("Crypto provider already configured")
        }
    });
}

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
    ensure_crypto_provider();

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
