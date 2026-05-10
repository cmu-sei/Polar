//! One-shot dev environment setup for local testing without Kubernetes.
//!
//! Run once before starting the cert issuer and Cassini:
//!
//!   cargo run --bin cert-issuer-setup
//!
//! What it does:
//!   1. Generates a CA keypair and self-signed CA cert -> dev/tmp/
//!   2. Generates an RSA signing keypair for the fake OIDC issuer
//!   3. Writes a JWKS document for that keypair -> dev/jwks.json
//!   4. Mints two tokens:
//!      - dev/token-cassini  (server cert, serverAuth + clientAuth EKU)
//!      - dev/token-agent    (client cert, clientAuth EKU only)
//!   5. Writes dev/config.json
//!   6. Prints the exact commands and env vars to run the full local stack
//!
//! In production each agent runs cert-issuer-init with its own
//! projected SA token. The dev/token-agent here is a stand-in for
//! that — same flow, generic identity. Swap it for any real SA token
//! to test a specific agent identity.
//!
//! Tokens expire after one hour. Re-run cert-issuer-setup to refresh.
//! CA materials in dev/tmp/ are preserved across re-runs — delete
//! dev/tmp/ explicitly only if you need to rotate the CA root, which
//! invalidates all outstanding certs.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, IsCa, KeyPair};
use rsa::pkcs8::EncodePrivateKey;
use rsa::traits::PublicKeyParts;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

const ISSUER: &str = "http://localhost:8081";
const AUDIENCE: &str = "polar-cert-issuer.dev";

const CASSINI_SUBJECT: &str = "system:serviceaccount:polar:cassini";
const CASSINI_DNS: &str = "cassini.polar.serviceaccount.cluster.local";

// Generic agent identity used as a stand-in for any real agent in dev.
// In production this is replaced by whatever SA the agent pod runs as.
const AGENT_SUBJECT: &str = "system:serviceaccount:polar:polar-agent";
const AGENT_DNS: &str = "polar-agent.polar.serviceaccount.cluster.local";

const TOKEN_LIFETIME_SECS: u64 = 3600;

fn main() {
    fs::create_dir_all("dev/tmp").expect("create dev/tmp");
    fs::create_dir_all("dev/certs/server").expect("create dev/certs/server");
    fs::create_dir_all("dev/certs/client").expect("create dev/certs/client");

    // ---- Step 1: CA --------------------------------------------------------

    println!("--- Step 1: generating CA keypair and self-signed cert ---");
    let (ca_cert_pem, ca_key_pem) = generate_ca();
    fs::write("dev/tmp/ca.crt", &ca_cert_pem).expect("write ca.crt");
    fs::write("dev/tmp/ca.key", &ca_key_pem).expect("write ca.key");
    println!("  wrote dev/tmp/ca.crt");
    println!("  wrote dev/tmp/ca.key");

    // ---- Step 2: OIDC signing keypair -------------------------------------

    println!("--- Step 2: generating OIDC issuer signing keypair ---");
    let mut rng = rsa::rand_core::OsRng;
    let oidc_private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).expect("RSA keypair generation");
    let oidc_public_key = oidc_private_key.to_public_key();

    // ---- Step 3: JWKS ------------------------------------------------------

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

    // ---- Step 4: tokens ----------------------------------------------------

    println!("--- Step 4: minting tokens ---");

    let cassini_token = mint_token(kid, &oidc_private_key, CASSINI_SUBJECT);
    fs::write("dev/token-cassini", &cassini_token).expect("write token-cassini");
    println!("  wrote dev/token-cassini");
    println!("    sub: {CASSINI_SUBJECT}");
    println!("    dns: {CASSINI_DNS}");

    let agent_token = mint_token(kid, &oidc_private_key, AGENT_SUBJECT);
    fs::write("dev/token-agent", &agent_token).expect("write token-agent");
    println!("  wrote dev/token-agent");
    println!("    sub: {AGENT_SUBJECT}");
    println!("    dns: {AGENT_DNS}");

    println!("  both tokens expire in {TOKEN_LIFETIME_SECS}s");

    // ---- Step 5: config ----------------------------------------------------

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
            "jwks_uri": "http://localhost:8081/jwks.json",
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

    // ---- Instructions ------------------------------------------------------

    println!();
    println!("=== Dev environment ready. Run in order: ===");
    println!();

    println!("  # Terminal 1: JWKS server (port 8081, free of Cassini on 8080)");
    println!("  python3 -m http.server 8081 --directory dev/");
    println!();

    println!("  # Terminal 2: cert issuer");
    println!("  cargo run --bin cert-issuer -- --config dev/config.json");
    println!();

    println!("  # Terminal 3: issue Cassini server cert (serverAuth + clientAuth)");
    println!("  cargo run --bin cert-issuer-init -- \\");
    println!("    --cert-issuer-url http://127.0.0.1:8443 \\");
    println!("    --token-path dev/token-cassini \\");
    println!("    --cert-dir dev/certs/server \\");
    println!("    --cert-type server");
    println!();

    println!("  # Terminal 3: issue generic agent client cert (clientAuth only)");
    println!("  cargo run --bin cert-issuer-init -- \\");
    println!("    --cert-issuer-url http://127.0.0.1:8443 \\");
    println!("    --token-path dev/token-agent \\");
    println!("    --cert-dir dev/certs/client");
    println!();

    println!("  # Cassini env vars:");
    println!("  export TLS_CA_CERT=$(pwd)/dev/tmp/ca.crt");
    println!("  export TLS_SERVER_CERT_CHAIN=$(pwd)/dev/certs/server/cert.pem");
    println!("  export TLS_SERVER_KEY=$(pwd)/dev/certs/server/key.pem");
    println!("  export BROKER_ADDR=127.0.0.1:8080");
    println!();

    println!("  # Agent env vars:");
    println!("  export TLS_CA_CERT=$(pwd)/dev/tmp/ca.crt");
    println!("  export TLS_CLIENT_CERT=$(pwd)/dev/certs/client/cert.pem");
    println!("  export TLS_CLIENT_KEY=$(pwd)/dev/certs/client/key.pem");
    println!("  export CASSINI_SERVER_NAME={CASSINI_DNS}");
    println!("  export BROKER_ADDR=127.0.0.1:8080");
    println!();

    println!("  # Verify EKUs:");
    println!(
        "  openssl x509 -in dev/certs/server/cert.pem -noout -text | grep -A5 'Extended Key Usage'"
    );
    println!(
        "  openssl x509 -in dev/certs/client/cert.pem -noout -text | grep -A5 'Extended Key Usage'"
    );
    println!();
    println!("  Tokens expire in {TOKEN_LIFETIME_SECS}s. Re-run cert-issuer-setup to refresh.");
    println!("  CA preserved across re-runs. Delete dev/tmp/ only to rotate the CA root.");
}

/// Generate a self-signed CA cert and key using rcgen.
///
/// Validity: 2026-01-01 to 2036-01-01.
fn generate_ca() -> (String, String) {
    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ED25519).expect("CA keypair generation");

    let mut params = CertificateParams::new(vec![]).expect("CertificateParams");
    params.distinguished_name = {
        let mut dn = DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, "Polar Internal CA");
        dn.push(rcgen::DnType::OrganizationName, "Polar");
        dn
    };
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.not_before = rcgen::date_time_ymd(2026, 1, 1);
    params.not_after = rcgen::date_time_ymd(2036, 1, 1);

    let cert = params.self_signed(&key_pair).expect("self-signed CA cert");
    (cert.pem(), key_pair.serialize_pem())
}

/// Mint an RS256 JWT for the given subject.
///
/// The sub claim is what the cert issuer normalizes into the cert's
/// DNS SAN. The kubernetes.io block is structurally correct but the
/// pod UID is fake — used for telemetry only, not issuance.
fn mint_token(kid: &str, key: &rsa::RsaPrivateKey, subject: &str) -> String {
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

    let parts: Vec<&str> = subject.splitn(4, ':').collect();
    let (namespace, name) = if parts.len() == 4 {
        (parts[2], parts[3])
    } else {
        ("polar", "unknown")
    };

    let claims = serde_json::json!({
        "iss": ISSUER,
        "aud": AUDIENCE,
        "sub": subject,
        "iat": now,
        "exp": now + TOKEN_LIFETIME_SECS,
        "kubernetes.io": {
            "namespace": namespace,
            "pod": {
                "name": format!("{name}-dev"),
                "uid": "00000000-0000-0000-0000-000000000001"
            }
        }
    });

    encode(&header, &claims, &encoding_key).expect("encode JWT")
}
