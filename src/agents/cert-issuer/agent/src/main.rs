//! Cert issuer binary entry point.
//!
//! Loads config, validates it, constructs the service components,
//! and serves HTTPS.
//!
//! # Configuration
//!
//! Config path comes from `CERT_ISSUER_CONFIG` env var, expected
//! to point at a JSON file matching the `ServiceConfig` schema.
//! The CA provisioner key is loaded from the path given in the
//! `ca.provisioner_key_path` field of the config.
//!
//! The intent is that `CERT_ISSUER_CONFIG` is the only thing the
//! deployment manifest needs to know — everything else flows from
//! that file. In production the config file itself is a mounted
//! Kubernetes secret or a Vault-rendered template; in development
//! it's a JSON file you write by hand.
//!
//! # TLS
//!
//! The cert issuer's serving certificate is loaded from paths
//! given in `tls.cert_path` and `tls.key_path` (added to the config
//! when TLS is needed). v1 omits TLS termination from this
//! binary — we expect a TLS-terminating proxy in front (Envoy,
//! nginx-ingress, or a service mesh sidecar). When/if we need
//! native TLS in this binary, we'll add a `tls` section to the
//! config and use `axum-server`'s TLS support. For now, plain HTTP
//! on the bind address is what we serve, and the deployment is
//! responsible for TLS.

use anyhow::{Context, Result};
use cert_issuer::ca::StepCaClient;
use cert_issuer::config::ServiceConfig;
use cert_issuer::handler::Handler;
use cert_issuer::oidc::Validator;
use cert_issuer::server::build_router;
use std::sync::Arc;
use tracing::{debug, info};

#[tokio::main]
async fn main() -> Result<()> {
    polar::init_logging("polar.certificates.issuer".to_string());

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

    // ---- CA client ----
    //
    // The provisioner key is loaded from disk per the config. In
    // production this path points at a mounted Kubernetes secret
    // (or a Vault-rendered file). The key bytes never leave the
    // process after this point.
    debug!(
        "Reading provisioner key PEM file at {}",
        config.ca.provisioner_key_path
    );

    let provisioner_key_pem =
        std::fs::read(&config.ca.provisioner_key_path).with_context(|| {
            format!(
                "reading provisioner key from {}",
                config.ca.provisioner_key_path
            )
        })?;

    let provisioner_alg = match config.ca.provisioner_alg.as_str() {
        "ES256" => jsonwebtoken::Algorithm::ES256,
        "EdDSA" => jsonwebtoken::Algorithm::EdDSA,
        other => {
            anyhow::bail!("unsupported provisioner_alg '{other}'; v1 supports ES256 and EdDSA")
        }
    };

    debug!("Provisioner configured with {provisioner_alg:?} Algorithm");

    debug!("Initializing CA Client...");
    let ca = StepCaClient::new(
        config.ca.url.clone(),
        config.ca.provisioner.clone(),
        &provisioner_key_pem,
        provisioner_alg,
    )
    .map_err(|e| anyhow::anyhow!("constructing CA client: {e}"))?;

    let ca_arc = Arc::new(ca);

    // ---- OIDC validator ----
    let validator = Validator::new(config.issuer.clone());

    // ---- Handler ----
    let handler = Arc::new(Handler {
        validator: Arc::new(validator),
        ca: ca_arc,
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
