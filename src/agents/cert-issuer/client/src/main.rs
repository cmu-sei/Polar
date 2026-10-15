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

// cert-issuer-init/src/main.rs
use cert_client::keypair::KeyAlgorithm;
use cert_client::{handshake, keypair, output, token};
use cert_issuer_common::{
    CertType,
    identity::{IdentityNormalization, normalize_identity},
};
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
    /// Path to the Polar CA certificate (PEM) to verify the cert issuer
    /// against from the first request. If omitted, the CA is fetched once
    /// from GET /ca without verification and pinned -- the degraded mode.
    #[arg(long)]
    ca_cert: Option<String>,
    /// Directory to write cert.pem, key.pem, and ca.pem into
    #[arg(long, default_value = "/etc/tls/certs")]
    cert_dir: String,
    /// Certificate type: CLIENT or SERVER
    /// TODO: see how we can turn this into a flag instead
    #[arg(long, default_value_t = CertType::Client)]
    cert_type: CertType,
    #[arg(long, default_value = "ed25519")]
    key_algorithm: KeyAlgorithm,
    /// The cluster's own DNS domain, used to scope the identity SAN
    /// derived from the SA token. Kubernetes' own default is
    /// "cluster.local"; a real cluster's kubelet can be configured
    /// with a different --cluster-domain, so this must be overridable
    /// per deployment rather than assumed.
    #[arg(long, default_value = "cluster.local")]
    cluster_domain: String,
    /// Additional DNS SANs to include in the certificate alongside the
    /// identity SAN derived from the SA token. May be specified multiple
    /// times. Wildcards and IP addresses are rejected.
    #[arg(long = "extra-san", value_name = "DNS_NAME")]
    extra_sans: Vec<String>,
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
    let strategy = IdentityNormalization::KubernetesServiceAccount {
        cluster_domain: args.cluster_domain.clone(),
    };
    let dns_identity = match normalize_identity(&sub, None, &strategy) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("error: failed to normalize identity '{sub}': {e}");
            return 1;
        }
    };
    let csr_output =
        match keypair::generate_csr(&dns_identity, &args.key_algorithm, &args.extra_sans) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: failed to generate CSR: {e}");
                return 3;
            }
        };
    let client = match handshake::HandshakeClient::for_trust_mode(
        args.cert_issuer_url,
        args.ca_cert.as_deref(),
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot establish trust in cert issuer: {e}");
            return 2;
        }
    };
    let response = match client
        .issue(
            &sa_token,
            &csr_output.csr_pem,
            args.cert_type,
            args.extra_sans,
            None,
        )
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
