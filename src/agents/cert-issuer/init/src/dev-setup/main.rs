//! One-shot dev environment setup for local testing without Kubernetes.
//!
//! Run once before starting the cert issuer and init container:
//!
//!   cargo run --bin dev-setup
//!
//! What it does:
//!   1. Generates a CA keypair and self-signed CA cert -> dev/tmp/
//!   2. Generates an Ed25519 signing keypair for the fake OIDC issuer
//!   3. Writes a JWKS document for that keypair -> dev/jwks.json
//!   4. Mints a JWT signed with that keypair -> dev/token
//!   5. Writes dev/config.json matching the cert issuer's expected shape
//!   6. Prints the commands to run the cert issuer and init container
//!
//! After running dev-setup:
//!
//!   # Terminal 1: serve the JWKS (Python is fine, it's dev)
//!   python3 -m http.server 8080 --directory dev/
//!
//!   # Terminal 2: run the cert issuer
//!   cargo run --bin cert-issuer -- --config dev/config.json
//!
//!   # Terminal 3: run the init container
//!   POLAR_SA_TOKEN_PATH=dev/token \
//!   POLAR_CERT_ISSUER_URL=http://127.0.0.1:8443 \
//!   POLAR_CERT_DIR=dev/certs \
//!   cargo run --bin polar-nu-init
//!
//! The generated keypairs and token are not secrets — they have no
//! real privileges and are only valid against the local dev cert issuer.
//! They are safe to commit as dev fixtures if the team wants a
//! reproducible dev environment without running dev-setup every time.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, IsCa, KeyPair};
use rsa::pkcs8::EncodePrivateKey;
use rsa::traits::PublicKeyParts;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

const ISSUER: &str = "http://localhost:8080";
const AUDIENCE: &str = "polar-cert-issuer.dev";
const SUBJECT: &str = "system:serviceaccount:polar:git-observer";
// Token lifetime: 1 hour. Long enough for a local dev session.
// Re-run dev-setup if the token expires.
const TOKEN_LIFETIME_SECS: u64 = 3600;

fn main() {
    fs::create_dir_all("dev/tmp").expect("create dev/tmp");
    fs::create_dir_all("dev/certs").expect("create dev/certs");

    println!("--- Step 1: generating CA keypair and self-signed cert ---");
    let (ca_cert_pem, ca_key_pem) = generate_ca();
    fs::write("dev/tmp/ca.crt", &ca_cert_pem).expect("write ca.crt");
    fs::write("dev/tmp/ca.key", &ca_key_pem).expect("write ca.key");
    println!("  wrote dev/tmp/ca.crt");
    println!("  wrote dev/tmp/ca.key");

    println!("--- Step 2: generating OIDC issuer signing keypair ---");
    // RSA 2048 because the JWKS format for RSA is well-supported by
    // jsonwebtoken and straightforward to encode as a JWK. Ed25519
    // JWK encoding is less standardized across libraries.
    let mut rng = rsa::rand_core::OsRng;
    let oidc_private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).expect("RSA keypair generation");
    let oidc_public_key = oidc_private_key.to_public_key();

    println!("--- Step 3: writing JWKS ---");
    let kid = "dev-key-1";
    let n = URL_SAFE_NO_PAD.encode(oidc_public_key.n().to_bytes_be());
    let e = URL_SAFE_NO_PAD.encode(oidc_public_key.e().to_bytes_be());
    let jwks = serde_json::json!({
        "keys": [{
            "kty": "RSA",
            "kid": kid,
            "use": "sig",
            "alg": "RS256",
            "n": n,
            "e": e,
        }]
    });
    fs::write("dev/jwks.json", serde_json::to_vec_pretty(&jwks).unwrap()).expect("write jwks.json");
    println!("  wrote dev/jwks.json");

    println!("--- Step 4: minting JWT ---");
    let token = mint_token(kid, &oidc_private_key);
    fs::write("dev/token", &token).expect("write token");
    println!("  wrote dev/token");
    println!("  sub:     {SUBJECT}");
    println!("  iss:     {ISSUER}");
    println!("  aud:     {AUDIENCE}");
    println!("  expires: {} seconds from now", TOKEN_LIFETIME_SECS);

    println!("--- Step 5: writing config ---");
    let config = serde_json::json!({
        "bind_addr": "127.0.0.1:8443",
        "ca": {
            "ca_cert_path": "./dev/tmp/ca.crt",
            "ca_key_path": "./dev/tmp/ca.key",
            "default_lifetime": { "secs": 1800, "nanos": 0 }
        },
        "issuer": {
            "issuer": ISSUER,
            "audience": AUDIENCE,
            "jwks_uri": "http://localhost:8080/jwks.json",
            "workload_identity_claim": "sub",
            "instance_binding_claim": "kubernetes.io/pod/uid",
            "allowed_algorithms": ["RS256", "ES256", "EdDSA"],
            "jwks_cache_ttl_min": { "secs": 30, "nanos": 0 },
            "jwks_cache_ttl_max": { "secs": 3600, "nanos": 0 }
        }
    });
    fs::write(
        "dev/config.json",
        serde_json::to_vec_pretty(&config).unwrap(),
    )
    .expect("write config.json");
    println!("  wrote dev/config.json");

    println!();
    println!("=== Dev environment ready. Run in three terminals: ===");
    println!();
    println!("  # Terminal 1: JWKS server");
    println!("  python3 -m http.server 8080 --directory dev/");
    println!();
    println!("  # Terminal 2: cert issuer");
    println!("  cargo run --bin cert-issuer -- --config dev/config.json");
    println!();
    println!("  # Terminal 3: init container");
    println!("  POLAR_SA_TOKEN_PATH=dev/token \\");
    println!("  POLAR_CERT_ISSUER_URL=http://127.0.0.1:8443 \\");
    println!("  POLAR_CERT_DIR=dev/certs \\");
    println!("  cargo run --bin polar-nu-init");
    println!();
    println!("  # Inspect output");
    println!("  openssl x509 -in dev/certs/cert.pem -text -noout");
}

/// Generate a self-signed CA cert and key using rcgen.
/// Returns (cert_pem, key_pem).
fn generate_ca() -> (String, String) {
    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ED25519).expect("CA keypair generation");

    let mut params = CertificateParams::new(vec![]).expect("CertificateParams");
    params.distinguished_name = {
        let mut dn = DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, "Polar Dev CA");
        dn.push(rcgen::DnType::OrganizationName, "Polar Dev");
        dn
    };
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    // 90 day CA cert for dev. Long enough to not get in the way.
    params.not_before = rcgen::date_time_ymd(2024, 1, 1);
    params.not_after = rcgen::date_time_ymd(2026, 1, 1);

    let cert = params.self_signed(&key_pair).expect("self-signed CA cert");

    (cert.pem(), key_pair.serialize_pem())
}

/// Mint an RS256 JWT with the claims the cert issuer expects.
fn mint_token(kid: &str, key: &rsa::RsaPrivateKey) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_secs();

    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.to_string());

    let pem = key
        .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .expect("to_pkcs8_pem");
    let encoding_key = EncodingKey::from_rsa_pem(pem.as_bytes()).expect("encoding key");

    let claims = serde_json::json!({
        "iss": ISSUER,
        "aud": AUDIENCE,
        "sub": SUBJECT,
        "iat": now,
        "exp": now + TOKEN_LIFETIME_SECS,
        // Include the kubernetes.io claim block so the cert issuer
        // can extract the instance binding claim. The pod UID is
        // fake but structurally correct.
        "kubernetes.io": {
            "namespace": "polar",
            "pod": {
                "name": "git-observer-dev",
                "uid": "00000000-0000-0000-0000-000000000001"
            }
        }
    });

    encode(&header, &claims, &encoding_key).expect("encode JWT")
}
