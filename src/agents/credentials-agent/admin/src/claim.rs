//! `claim`: take possession of a provisioned account.
//!
//! An admin provisions an account by username and email; the account is not
//! usable until its holder claims it. This is the native flavor of the same
//! two-phase `/account` flow `bootstrap` uses, with username and email as the
//! claimant instead of a Kubernetes token. The consumer matches them against
//! the provisioning record, offers the account id, and signs the CSR we bind
//! to that id.
//!
//! The keypair is generated here and never leaves this machine. As with
//! `bootstrap`, the key is written to disk BEFORE the certificate is
//! requested, so a crash after issuance cannot orphan the certificate, and
//! the issued certificate is checked against the key before success is
//! reported. Output paths must not already exist.

use crate::bootstrap::{emit_fallback, post, write_new};
use cert_issuer_common::key_match::certificate_matches_key;
use cert_issuer_common::keypair::{KeyAlgorithm, generate_csr};
use credentials_agent_common::account_claim::{AccountReply, AccountRequest, Claimant};
use std::path::PathBuf;

pub struct ClaimArgs {
    pub username: String,
    pub email: String,
    pub observer_url: String,
    pub cert_out: PathBuf,
    pub key_out: PathBuf,
}

pub async fn run(args: ClaimArgs) -> Result<(), String> {
    for path in [&args.cert_out, &args.key_out] {
        if path.exists() {
            return Err(format!(
                "{} already exists. Refusing to overwrite it.",
                path.display()
            ));
        }
    }

    let http = reqwest::Client::new();
    let url = format!("{}/account", args.observer_url.trim_end_matches('/'));
    let claimant = Claimant::Native {
        username: args.username.clone(),
        email: args.email.clone(),
    };

    // Phase 1: the consumer matches username and email against the
    // provisioning record and offers the account id.
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
    eprintln!("account: {account_id}");

    // Key first, to disk, before anything irreversible happens.
    let csr = generate_csr(&account_id.to_string(), &KeyAlgorithm::EcdsaP256, &[])
        .map_err(|e| format!("cannot generate CSR: {e}"))?;
    write_new(&args.key_out, csr.private_key_pem.as_bytes(), 0o600)?;
    eprintln!("private key written to {}", args.key_out.display());

    // Phase 2: the CSR, plus the claim again.
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
        return Err(format!("{e}; the certificate was printed to stdout instead"));
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
    eprintln!("Back up {} -- it is your identity.", args.key_out.display());
    eprintln!();
    eprintln!("To import into a browser, bundle it as PKCS#12:");
    eprintln!(
        "  openssl pkcs12 -export -inkey {} -in {} -certfile {} -out {}",
        args.key_out.display(),
        args.cert_out.display(),
        chain_path.display(),
        args.cert_out.with_extension("p12").display()
    );
    Ok(())
}
