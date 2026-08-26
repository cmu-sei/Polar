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
    health,
    oidc::Validator,
    server::{build_router, health_routes},
};
use std::sync::Arc;
use tracing::{debug, info, instrument};

#[tokio::main]
#[instrument]
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
        audience = %config.issuer.audience.join(", "),
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
    // CA load failure is a hard startup error by design — see
    // health.rs module docs. There is no runtime "CA became
    // unavailable" state for readiness to represent, because if this
    // call fails the process must never reach a state where it's
    // accepting connections at all.
    .context("CA materials")?;

    // begin tracking health state once ca loads
    let health = health::HealthState::new(config.issuer.jwks_cache_ttl_max);
    health.mark_ca_loaded();

    let ca = RcgenCaClient::new(&ca_cert_pem, &ca_key_pem)
        .map_err(|e| anyhow::anyhow!("constructing CA client: {e}"))?;
    debug!("Internal certificate authroity initialized.");

    // ---- OIDC validator ----
    let validator = Arc::new(Validator::new(config.issuer.clone(), Arc::clone(&health)));
    debug!("OIDC validator initialized.");

    // Break the readiness/Service-routing deadlock: fetch JWKS once
    // here, driven by this process itself rather than by an inbound
    // request the Service won't route until the pod is ready. Runs
    // in the background so a slow or briefly-unreachable JWKS
    // endpoint doesn't delay binding the HTTP port — liveness must
    // stay independent of this. Retries with bounded exponential
    // backoff rather than fetching once and giving up, since the
    // Kubernetes API server may not be reachable in the first few
    // seconds after this pod starts (e.g. during a simultaneous
    // rollout of core cluster components).
    {
        let validator = Arc::clone(&validator);
        tokio::spawn(async move {
            let mut backoff = std::time::Duration::from_secs(1);
            const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);
            loop {
                match validator.warm_jwks_cache().await {
                    Ok(()) => {
                        info!("JWKS cache warmed at startup");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, next_retry = ?backoff, "JWKS warm-up failed, retrying");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                    }
                }
            }
        });
    }
    // ---- Handler ----
    let handler = Arc::new(Handler {
        validator: Arc::clone(&validator),
        ca: Arc::new(ca),
        default_lifetime: config.ca.default_lifetime,
        server_lifetime: config.ca.server_lifetime,
        identity_lifetime_overrides: config.ca.identity_lifetime_overrides.clone(),
    });

    // ---- Server ----
    let app = build_router(handler).merge(health_routes(health));
    let listener = tokio::net::TcpListener::bind(&config.bind_addr)
        .await
        .with_context(|| format!("binding {}", config.bind_addr))?;

    info!(bind_addr = %config.bind_addr, "listening");
    axum::serve(listener, app).await.context("axum serve")?;

    Ok(())
}
