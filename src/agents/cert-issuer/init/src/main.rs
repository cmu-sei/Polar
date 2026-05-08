// cert-issuer-init/src/main.rs

use cert_issuer_common::{CertType, identity::normalize_identity};
use cert_issuer_init::{handshake, keypair, output, token};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "cert-issuer-init",
    about = "Obtain a short-lived mTLS certificate from the Polar cert issuer"
)]
struct Args {
    /// URL of the cert issuer service
    #[arg(long)]
    cert_issuer_url: String,

    /// Path to the projected service account token
    #[arg(long, default_value = "/workspace/token")]
    token_path: String,

    /// Directory to write cert.pem, key.pem, and ca.pem into
    #[arg(long, default_value = "/etc/tls/certs")]
    cert_dir: String,

    /// Certificate type: CLIENT or SERVER
    /// TODO: see how we can turn this into a flag instead
    #[arg(long, default_value_t = CertType::Client)]
    cert_type: CertType,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    std::process::exit(run(args).await);
}

async fn run(args: Args) -> i32 {
    let sa_token = match token::read_token(&args.token_path) {
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
            eprintln!("error: failed to normalize identity '{sub}': {e}");
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

    let client = handshake::HandshakeClient::new(args.cert_issuer_url);
    let response = match client
        .issue(&sa_token, &csr_output.csr_pem, args.cert_type)
        .await
    {
        Ok(r) => r,
        Err(handshake::HandshakeError::Unreachable(e)) => {
            eprintln!("error: cert issuer unreachable: {e}");
            return 2;
        }
        Err(handshake::HandshakeError::Rejected(e)) => {
            eprintln!(
                "error: cert issuer rejected: {:?} — {}",
                e.outcome, e.detail
            );
            return match e.outcome {
                cert_issuer_common::IssueOutcome::InvalidAudience => 1,
                cert_issuer_common::IssueOutcome::InvalidToken => 1,
                cert_issuer_common::IssueOutcome::IdentityMismatch => 1,
                cert_issuer_common::IssueOutcome::CaUnavailable => 2,
                _ => 3,
            };
        }
        Err(handshake::HandshakeError::Malformed(e)) => {
            eprintln!("error: malformed response: {e}");
            return 3;
        }
    };

    let bundle = output::OutputBundle {
        cert_pem: &response.certificate_pem,
        key_pem: &csr_output.private_key_pem,
        ca_pem: &response.ca_chain_pem,
    };

    match output::write_bundle(std::path::Path::new(&args.cert_dir), &bundle) {
        Ok(()) => {
            eprintln!(
                "issued cert for '{dns_identity}', expires {}",
                response.expires_at
            );
            0
        }
        Err(e) => {
            eprintln!("error: failed to write cert bundle: {e}");
            3
        }
    }
}
