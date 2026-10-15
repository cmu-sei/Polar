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

//! `bootstrap`: the one-time claim of the bootstrap admin account.
//!
//! The cluster starts with no admin and no way to sign an admin action.
//! This subcommand proves, with a Kubernetes ServiceAccount token, that the
//! caller controls the cluster, and receives the bootstrap admin
//! certificate. It goes through the observer's HTTP `/account` route (the
//! observer relays to the credentials agent over Cassini), because this
//! tool has no client certificate yet and so cannot reach Cassini itself.
//!
//! The claim succeeds exactly once. If the certificate or key is lost, the
//! only recovery is rebuilding the cluster. So:
//!   * both output paths must not exist before we start;
//!   * the private key is written to disk BEFORE the certificate is
//!     requested, so a crash after issuance cannot orphan the certificate;
//!   * the issued certificate is checked against the key before success is
//!     reported, and printed to stdout if it cannot be written.

use cert_issuer_common::key_match::certificate_matches_key;
use cert_issuer_common::keypair::{KeyAlgorithm, generate_csr};
use credentials_agent_common::account_claim::{AccountReply, AccountRequest, Claimant};
use k8s_openapi::api::authentication::v1::{TokenRequest, TokenRequestSpec};
use k8s_openapi::api::core::v1::ServiceAccount;
use kube::Client;
use kube::api::{Api, PostParams};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct BootstrapArgs {
    pub namespace: String,
    pub service_account: String,
    pub audience: String,
    pub observer_url: String,
    pub cert_out: PathBuf,
    pub key_out: PathBuf,
}

pub async fn run(args: BootstrapArgs) -> Result<(), String> {
    for path in [&args.cert_out, &args.key_out] {
        if path.exists() {
            return Err(format!(
                "{} already exists. Refusing to overwrite: the bootstrap certificate \
                 can be issued only once and cannot be recovered.",
                path.display()
            ));
        }
    }

    let token = mint_token(&args).await?;
    let http = reqwest::Client::new();
    let url = format!("{}/account", args.observer_url.trim_end_matches('/'));
    let claimant = Claimant::Bootstrap { credential: token };

    // Phase 1: prove the identity, learn the account id.
    let account_id = match post(
        &http,
        &url,
        &AccountRequest::Begin {
            claimant: claimant.clone(),
        },
    )
    .await?
    {
        AccountReply::Offered { account_id } => account_id,
        other => return Err(format!("unexpected reply to Begin: {other:?}")),
    };
    eprintln!("bootstrap account: {account_id}");

    // Key first, to disk, before anything irreversible happens.
    let csr = generate_csr(&account_id.to_string(), &KeyAlgorithm::Ed25519, &[])
        .map_err(|e| format!("cannot generate CSR: {e}"))?;
    write_new(&args.key_out, csr.private_key_pem.as_bytes(), 0o600)?;
    eprintln!("private key written to {}", args.key_out.display());

    // Phase 2: the CSR, plus the proof again.
    let (certificate_pem, ca_chain_pem) = match post(
        &http,
        &url,
        &AccountRequest::Complete {
            claimant,
            csr_pem: csr.csr_pem,
        },
    )
    .await?
    {
        AccountReply::Issued {
            certificate_pem,
            ca_chain_pem,
        } => (certificate_pem, ca_chain_pem),
        other => return Err(format!("unexpected reply to Complete: {other:?}")),
    };

    match certificate_matches_key(&certificate_pem, &csr.private_key_pem) {
        Ok(true) => {}
        Ok(false) => {
            emit_fallback(&certificate_pem, &ca_chain_pem);
            return Err("the issued certificate does not match the generated key".to_string());
        }
        Err(e) => {
            emit_fallback(&certificate_pem, &ca_chain_pem);
            return Err(format!("cannot verify the issued certificate: {e}"));
        }
    }

    if let Err(e) = write_new(&args.cert_out, certificate_pem.as_bytes(), 0o644) {
        emit_fallback(&certificate_pem, &ca_chain_pem);
        return Err(format!(
            "{e}; the certificate was printed to stdout instead"
        ));
    }
    let chain_path = args.cert_out.with_extension("chain.pem");
    if let Err(e) = write_new(&chain_path, ca_chain_pem.as_bytes(), 0o644) {
        eprintln!(
            "warning: could not write the CA chain to {}: {e}",
            chain_path.display()
        );
    }

    eprintln!("certificate written to {}", args.cert_out.display());
    eprintln!();
    eprintln!("!! This is the ONLY bootstrap certificate that will ever be issued.");
    eprintln!(
        "!! Back up {} and {} now.",
        args.cert_out.display(),
        args.key_out.display()
    );
    eprintln!("!! If either is lost, the only recovery is rebuilding the cluster.");
    eprintln!();
    eprintln!(
        "Next: polar-admin-grant --cert {} --key {} provision-account ...",
        args.cert_out.display(),
        args.key_out.display()
    );
    Ok(())
}

/// A short-lived token for the bootstrap ServiceAccount, minted with the
/// operator's own kubeconfig. Anyone who can mint it controls the cluster.
async fn mint_token(args: &BootstrapArgs) -> Result<String, String> {
    let client = Client::try_default()
        .await
        .map_err(|e| format!("cannot build a Kubernetes client: {e}"))?;
    let accounts: Api<ServiceAccount> = Api::namespaced(client, &args.namespace);
    let request = TokenRequest {
        spec: TokenRequestSpec {
            audiences: vec![args.audience.clone()],
            expiration_seconds: Some(600),
            ..Default::default()
        },
        ..Default::default()
    };
    let issued = accounts
        .create_token_request(&args.service_account, &PostParams::default(), &request)
        .await
        .map_err(|e| {
            format!(
                "cannot mint a token for {}/{}: {e}",
                args.namespace, args.service_account
            )
        })?;
    issued
        .status
        .map(|s| s.token)
        .ok_or_else(|| "the API server returned no token".to_string())
}

pub(crate) async fn post(
    http: &reqwest::Client,
    url: &str,
    request: &AccountRequest,
) -> Result<AccountReply, String> {
    let response = http
        .post(url)
        .json(request)
        .send()
        .await
        .map_err(|e| format!("cannot reach the observer at {url}: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("the observer answered {status}: {body}"));
    }
    response
        .json::<AccountReply>()
        .await
        .map_err(|e| format!("unreadable reply from the observer: {e}"))
}

/// Creates the file, failing if it exists, with the given Unix mode.
pub(crate) fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

pub(crate) fn emit_fallback(certificate_pem: &str, ca_chain_pem: &str) {
    println!("{certificate_pem}");
    println!("{ca_chain_pem}");
}
