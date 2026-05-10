//! Cert issuer binary entry point.
//!
//! Loads config, validates it, ensures the CA materials are
//! present (loading if they exist, generating if they don't),
//! constructs the service components, and serves HTTP.
//!
//! # Configuration
//!
//! Config path comes from `CERT_ISSUER_CONFIG` env var, expected
//! to point at a JSON file matching the `ServiceConfig` schema.
//! The CA cert and private key are loaded from the paths given in
//! `ca.ca_cert_path` and `ca.ca_key_path`. If those files don't
//! exist on first startup, the service generates a fresh CA
//! keypair and writes them — see `ca::load_or_bootstrap_ca` for
//! the full state-handling rules.
//!
//! The intent is that `CERT_ISSUER_CONFIG` is the only thing the
//! deployment manifest needs to know — everything else flows from
//! that file. In production the CA materials live on a Kubernetes
//! Secret-backed volume that persists across pod restarts; in
//! development they're files in a local directory that the cert
//! issuer creates on first run.
//!
//! # TLS
//!
//! v1 omits TLS termination from this binary — we expect a
//! TLS-terminating proxy in front (Envoy, nginx-ingress, or a
//! service mesh sidecar). When/if we need native TLS in this
//! binary, we'll add a `tls` section to the config and use
//! `axum-server`'s TLS support.

use anyhow::{Context, Result};
use cert_issuer::{
    ca::{RcgenCaClient, load_or_bootstrap_ca},
    config::ServiceConfig,
    handler::Handler,
    oidc::Validator,
    server::build_router,
};
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    polar::init_logging("polar.cert-issuer.svc".to_string());
    // ---- Config ----
    let config_path = std::env::var("CERT_ISSUER_CONFIG")
        .context("CERT_ISSUER_CONFIG environment variable must be set")?;
    let config_bytes = std::fs::read(&config_path)
        .with_context(|| format!("reading config from {config_path}"))?;
    let config: ServiceConfig =
        serde_json::from_slice(&config_bytes).context("parsing config JSON")?;
    config.validate().context("config validation")?;

    info!(
        bind_addr = %config.bind_addr,
        issuer = %config.issuer.issuer,
        audience = %config.issuer.audience,
        "cert issuer starting"
    );

    // ---- CA materials ----
    //
    // Either load existing CA materials from disk, or generate a
    // fresh CA root if no materials exist yet. See
    // `load_or_bootstrap_ca` for the full state-handling rules:
    // partial state (only one of the two files present, key with
    // bad permissions, etc.) is a hard error rather than something
    // we silently paper over.
    let (ca_cert_pem, ca_key_pem) = load_or_bootstrap_ca(
        &config.ca.ca_cert_path,
        &config.ca.ca_key_path,
        // CA Common Name. Used only when bootstrapping; loaded CAs
        // keep whatever CN they were created with. Operators can
        // override this in the config if they care; the default is
        // descriptive enough for ad-hoc deployments.
        "Polar Internal CA",
    )
    .context("CA materials")?;

    let ca = RcgenCaClient::new(&ca_cert_pem, &ca_key_pem)
        .map_err(|e| anyhow::anyhow!("constructing CA client: {e}"))?;

    // ---- OIDC validator ----
    let validator = Validator::new(config.issuer.clone());

    // ---- Handler ----
    let handler = Arc::new(Handler {
        validator: Arc::new(validator),
        ca: Arc::new(ca),
        default_lifetime: config.ca.default_lifetime,
    });

    // ---- Server ----
    let app = build_router(handler);
    let listener = tokio::net::TcpListener::bind(&config.bind_addr)
        .await
        .with_context(|| format!("binding {}", config.bind_addr))?;

    info!(bind_addr = %config.bind_addr, "listening");
    axum::serve(listener, app).await.context("axum serve")?;

    Ok(())
}
