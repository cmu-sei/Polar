/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

//! user-cert-fetcher: the end-user tool for self-service certificate
//! issuance. Today's scope accepts an already-obtained credential via
//! a file (--credential-path) -- the full Authorization Code flow
//! against the IdP is deliberately deferred, the same pragmatic
//! approach used for the admin tool's own bootstrap step.

mod observer_client;

use cert_client::output::{self, OutputBundle};
use cert_issuer_common::keypair::{self, KeyAlgorithm};
use clap::Parser;
use credentials_agent_common::identity::IdentityNormalizationReply;
use credentials_agent_common::issuance::IssuanceReply;
use observer_client::{ObserverClient, ObserverClientError};
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(
    name = "user-cert-fetcher",
    about = "Obtain a short-lived mTLS certificate from the Credentials Agent"
)]
struct Args {
    /// URL of the Credentials Agent's observer.
    #[arg(long)]
    observer_url: String,
    /// Path to a file containing the credential (access token) to use.
    #[arg(long)]
    credential_path: String,
    /// Directory to write cert.pem, key.pem, and ca.pem into.
    #[arg(long, default_value = "/etc/tls/certs")]
    cert_dir: String,
    #[arg(long, default_value = "ed25519")]
    key_algorithm: KeyAlgorithm,
}

/// Up to 3 total attempts, per cassini-request-reply-protocol.md --
/// each a genuinely fresh correlation_id, entirely the observer's own
/// internal concern; this client never sees or manages one directly.
const MAX_ATTEMPTS: u32 = 3;
/// Hard ceiling: 5 minutes total, per the same spec.
const HARD_CEILING: Duration = Duration::from_secs(300);

#[tokio::main]
async fn main() {
    let args = Args::parse();
    std::process::exit(run(args).await);
}

async fn run(args: Args) -> i32 {
    let credential = match std::fs::read_to_string(&args.credential_path) {
        Ok(t) => t.trim().to_string(),
        Err(e) => {
            eprintln!(
                "error: failed to read credential from {}: {e}",
                args.credential_path
            );
            return 1;
        }
    };

    let client = ObserverClient::new(args.observer_url);

    // Step 1: identity normalization -- what SAN the CSR must carry
    // to match exactly what cert-issuer's /issue will check.
    let normalized_identity = match client.identity(&credential).await {
        Ok(IdentityNormalizationReply::Normalized { identity }) => identity,
        Ok(IdentityNormalizationReply::Refused { reason }) => {
            eprintln!("error: identity query refused: {reason:?}");
            return 1;
        }
        Err(e) => {
            eprintln!("error: identity query failed: {e}");
            return 2;
        }
    };

    // Step 2: generate keypair + CSR locally -- the private key never
    // leaves this machine.
    let csr_output = match keypair::generate_csr(&normalized_identity, &args.key_algorithm, &[]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: failed to generate CSR: {e}");
            return 3;
        }
    };

    // Step 3: submit for issuance. Retries only on genuine
    // infrastructure failures (timeouts, an unreachable observer) --
    // never on a real, deterministic refusal, which retrying would
    // never change.
    let start = Instant::now();
    let mut last_error = String::new();

    for attempt in 1..=MAX_ATTEMPTS {
        if start.elapsed() >= HARD_CEILING {
            eprintln!("error: exceeded the 5-minute retry ceiling");
            return 2;
        }

        match client.issue(&csr_output.csr_pem, &credential).await {
            Ok(IssuanceReply::Issued {
                certificate_pem,
                ca_chain_pem,
                ..
            }) => {
                let bundle = OutputBundle {
                    cert_pem: &certificate_pem,
                    key_pem: &csr_output.private_key_pem,
                    ca_pem: &ca_chain_pem,
                };
                return match output::write_bundle(std::path::Path::new(&args.cert_dir), &bundle) {
                    Ok(()) => {
                        eprintln!("issued cert for '{normalized_identity}'");
                        0
                    }
                    Err(e) => {
                        eprintln!("error: failed to write cert bundle: {e}");
                        3
                    }
                };
            }
            Ok(IssuanceReply::Refused { reason }) => {
                eprintln!("error: issuance refused: {reason:?}");
                return 1;
            }
            Err(ObserverClientError::Unreachable(e)) | Err(ObserverClientError::Retryable(e)) => {
                last_error = e;
                eprintln!("attempt {attempt}/{MAX_ATTEMPTS} failed: {last_error}, retrying...");
            }
        }
    }

    eprintln!("error: exhausted {MAX_ATTEMPTS} attempts, last error: {last_error}");
    2
}
