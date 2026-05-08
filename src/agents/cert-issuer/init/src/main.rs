// cert_issuer_init/src/main.rs

use cert_issuer_common::{IssueOutcome, identity::normalize_identity};

use cert_issuer_init::{
    handshake::{HandshakeClient, HandshakeError},
    keypair, output, token,
};

#[tokio::main]
async fn main() {
    std::process::exit(run().await);
}

async fn run() -> i32 {
    let token_path = std::env::var("POLAR_SA_TOKEN_PATH")
        .unwrap_or_else(|_| "/var/run/secrets/polar/token".to_string());
    let cert_issuer_url = match std::env::var("POLAR_CERT_ISSUER_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("error: POLAR_CERT_ISSUER_URL must be set");
            return 1;
        }
    };
    let cert_dir =
        std::env::var("POLAR_CERT_DIR").unwrap_or_else(|_| "/workspace/certs".to_string());

    let sa_token = match token::read_token(&token_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: failed to read SA token: {e}");
            return 1;
        }
    };

    let sub = match token::extract_sub(&sa_token) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to extract sub claim: {e}");
            return 1;
        }
    };

    let dns_identity = match normalize_identity(&sub) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("error: failed to normalize identity: {e}");
            return 1;
        }
    };

    let csr_output = match keypair::generate_csr(&dns_identity) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: failed to generate CSR: {e}");
            return 3;
        }
    };

    let client = HandshakeClient::new(cert_issuer_url);
    let response = match client.issue(&sa_token, &csr_output.csr_pem).await {
        Ok(r) => r,
        Err(HandshakeError::Unreachable(e)) => {
            eprintln!("error: cert issuer unreachable: {e}");
            return 2;
        }
        Err(HandshakeError::Rejected(e)) => {
            eprintln!(
                "error: cert issuer rejected request: {:?} — {}",
                e.outcome, e.detail
            );
            return match e.outcome {
                IssueOutcome::InvalidAudience => 1,
                IssueOutcome::InvalidToken => 1,
                IssueOutcome::IdentityMismatch => 1,
                IssueOutcome::CaUnavailable => 2,
                _ => 3,
            };
        }
        Err(HandshakeError::Malformed(e)) => {
            eprintln!("error: malformed response from cert issuer: {e}");
            return 3;
        }
    };

    let bundle = output::OutputBundle {
        cert_pem: &response.certificate_pem,
        key_pem: &csr_output.private_key_pem,
        ca_pem: &response.ca_chain_pem,
    };

    match output::write_bundle(std::path::Path::new(&cert_dir), &bundle) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: failed to write cert bundle: {e}");
            3
        }
    }
}
