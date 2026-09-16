//! polar-init
//!
//! Single init container binary for Polar agents.
//!
//! Replaces the two cert-client init containers with one binary that:
//!   1. Polls all specified dependency TLS endpoints until their server
//!      certificates have at least the required TTL remaining.
//!   2. Issues all required certificates via cert-issuer, retrying
//!      patiently until cert-issuer is available.
//!
//! The binary never exits non-zero due to transient failures — it
//! polls until the system is ready, then exits 0. The only non-zero
//! exits are for unrecoverable configuration errors (bad args, missing
//! SA token, etc.) that require operator intervention.
//!
//! Environment variables:
//!   POLAR_INIT_CERT_ISSUER_URL   URL of the cert issuer service (required)
//!   POLAR_INIT_TOKEN_PATH        Path to projected SA token
//!                                (default: /workspace/token)

use std::str::FromStr;
use std::time::Duration;

use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs};
use cert_client::{handshake, output, token};
use cert_issuer_common::keypair;
use cert_issuer_common::{
    CertType,
    identity::{IdentityNormalization, normalize_identity},
};
use clap::Parser;
use polar_health::DepCertEndpoint;
use ractor::{Actor, OutputPort};

const DEFAULT_TOKEN_PATH: &str = "/workspace/token";
const POLL_INTERVAL_SECS: u64 = 5;

/// If set (to any non-empty value), skips the escrow-keypair
/// bootstrap phase entirely -- required for the three core services
/// (the Credentials Agent itself, cert-issuer, and Cassini) whose own
/// polar-init instance would otherwise create a genuine, unresolvable
/// circularity: CryptoActor is the single gateway to the Credentials
/// Agent, so the Credentials Agent's own bootstrap can't depend on it
/// existing yet. See CryptoMessage::EnsureBootstrapped's own doc
/// comment (polar-crypto) for the complete, real reasoning behind
/// this phase existing at all.
const SKIP_ESCROW_BOOTSTRAP_ENV: &str = "POLAR_INIT_SKIP_ESCROW_BOOTSTRAP";

// ---------------------------------------------------------------------------
// CertRequest — parsed from --cert <type:dir:key-algorithm:extra-sans>
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct CertRequest {
    cert_type: CertType,
    cert_dir: String,
    key_algorithm: keypair::KeyAlgorithm,
    extra_sans: Vec<String>,
}

impl FromStr for CertRequest {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Format: type:dir:key-algorithm:extra-sans
        // extra-sans is comma-separated and may be empty.
        let parts: Vec<&str> = s.splitn(4, ':').collect();
        if parts.len() < 3 {
            return Err(format!(
                "invalid --cert '{}': expected type:dir:key-algorithm[:extra-sans]",
                s
            ));
        }

        let cert_type = match parts[0].trim().to_lowercase().as_str() {
            "client" => CertType::Client,
            "server" => CertType::Server,
            other => {
                return Err(format!(
                    "invalid cert type '{}': expected 'client' or 'server'",
                    other
                ))
            }
        };

        let cert_dir = parts[1].trim().to_string();
        if cert_dir.is_empty() {
            return Err(format!("invalid --cert '{}': cert_dir must not be empty", s));
        }

        let key_algorithm = match parts[2].trim().to_lowercase().as_str() {
            "ed25519" => keypair::KeyAlgorithm::Ed25519,
            "ecdsa-p256" => keypair::KeyAlgorithm::EcdsaP256,
            other => {
                return Err(format!(
                    "invalid key algorithm '{}': expected 'ed25519' or 'ecdsa-p256'",
                    other
                ))
            }
        };

        let extra_sans = if parts.len() == 4 && !parts[3].trim().is_empty() {
            parts[3]
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        } else {
            vec![]
        };

        Ok(CertRequest {
            cert_type,
            cert_dir,
            key_algorithm,
            extra_sans,
        })
    }
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "polar-init",
    about = "Polar agent init container: wait for dep cert health, then issue certs"
)]
struct Args {
    /// Dependency TLS endpoint to check before issuing certs.
    /// Format: host:port:min_ttl_seconds
    /// May be specified multiple times.
    #[arg(long = "check-dep-cert", value_name = "HOST:PORT:TTL_SECS")]
    dep_certs: Vec<String>,

    /// Certificate to issue.
    /// Format: type:dir:key-algorithm[:extra-sans]
    /// where extra-sans is a comma-separated list of DNS names.
    /// May be specified multiple times.
    #[arg(long = "cert", value_name = "TYPE:DIR:ALGORITHM[:SANS]")]
    certs: Vec<String>,

    /// The cluster's own DNS domain, used to scope the identity SAN
    /// derived from the SA token. Kubernetes' own default is
    /// "cluster.local"; a real cluster's kubelet can be configured
    /// with a different --cluster-domain, so this must be overridable
    /// per deployment rather than assumed.
    #[arg(long, default_value = "cluster.local")]
    cluster_domain: String,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    // Install aws-lc-rs as the default Rustls crypto provider.
    // Required when multiple providers are present in the dependency tree.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();

    let args = Args::parse();

    // Validate and parse all args upfront — fail fast on bad config.
    let dep_endpoints: Vec<DepCertEndpoint> = match args
        .dep_certs
        .iter()
        .map(|s| DepCertEndpoint::parse(s))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(deps) => deps,
        Err(e) => {
            eprintln!("error: invalid --check-dep-cert argument: {e}");
            std::process::exit(1);
        }
    };

    let cert_requests: Vec<CertRequest> = match args
        .certs
        .iter()
        .map(|s| s.parse::<CertRequest>())
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(certs) => certs,
        Err(e) => {
            eprintln!("error: invalid --cert argument: {e}");
            std::process::exit(1);
        }
    };

    if cert_requests.is_empty() {
        eprintln!("error: at least one --cert argument is required");
        std::process::exit(1);
    }

    let cert_issuer_url = match std::env::var("POLAR_INIT_CERT_ISSUER_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("error: POLAR_INIT_CERT_ISSUER_URL must be set and non-empty");
            std::process::exit(1);
        }
    };

    let token_path = std::env::var("POLAR_INIT_TOKEN_PATH")
        .unwrap_or_else(|_| DEFAULT_TOKEN_PATH.to_string());

    // Phase 1: wait for all dep cert endpoints to be healthy.
    if !dep_endpoints.is_empty() {
        wait_for_dep_certs(&dep_endpoints).await;
    }

    // Phase 2: read and parse the SA token — shared across all cert requests.
    let sa_token = loop {
        match token::read_token(&token_path) {
            Ok(t) => break t,
            Err(e) => {
                tracing::warn!("failed to read SA token at '{token_path}': {e}, retrying...");
                tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
            }
        }
    };

    let sub = match token::extract_sub(&sa_token) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to extract sub claim from SA token: {e}");
            std::process::exit(1);
        }
    };

    let strategy = IdentityNormalization::KubernetesServiceAccount {
        cluster_domain: args.cluster_domain.clone(),
    };

    let dns_identity = match normalize_identity(&sub, None, &strategy) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("error: failed to normalize identity '{sub}': {e}");
            std::process::exit(1);
        }
    };

    tracing::info!("identity: {dns_identity}");

    // Phase 3: issue all requested certs.
    for req in &cert_requests {
        issue_cert(&cert_issuer_url, &sa_token, &dns_identity, req).await;
    }

    tracing::info!("all certs issued");

    // Phase 4: ensure this identity's own escrow keypair genuinely
    // exists and is escrowed with the Credentials Agent -- skipped
    // entirely for the three core services (see
    // SKIP_ESCROW_BOOTSTRAP_ENV's own doc comment for why). This is
    // the only point in this agent's entire lifecycle where the SA
    // token read above is ever readable at all; the main, long-
    // running agent container never has it mounted.
    let skip_escrow_bootstrap = std::env::var(SKIP_ESCROW_BOOTSTRAP_ENV)
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    if skip_escrow_bootstrap {
        tracing::info!(
            "{SKIP_ESCROW_BOOTSTRAP_ENV} is set, skipping escrow-keypair bootstrap"
        );
    } else {
        ensure_escrow_bootstrapped(&dns_identity).await;
    }

    tracing::info!("polar-init complete, handing off to main container");
}

/// Establishes a real, standalone Cassini connection (using the
/// just-issued client-type mTLS cert) and a real CryptoActor
/// instance, sends EnsureBootstrapped, and retries until it succeeds
/// -- matching this binary's own, established "never exit non-zero
/// due to a transient failure" philosophy, since this genuinely
/// depends on the Credentials Agent specifically being reachable,
/// not just cert-issuer.
async fn ensure_escrow_bootstrapped(own_identity: &str) {
    // Reads TLS_CA_CERT/TLS_CLIENT_CERT/TLS_CLIENT_KEY/BROKER_ADDR/
    // CASSINI_SERVER_NAME from the environment -- the exact same,
    // established commonClientEnv every agent's own main container
    // already gets (infra/schema/constants.dhall), now also present
    // on this init container's own env (see makePolarInitContainer's
    // own, real Dhall definition).
    let client_config = loop {
        match TCPClientConfig::new() {
            Ok(config) => break config,
            Err(e) => {
                tracing::warn!("Cassini client config not yet ready: {e}, retrying...");
                tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
            }
        }
    };

    let events_output = std::sync::Arc::new(OutputPort::default());

    let (tcp_client, _) = loop {
        match Actor::spawn(
            None,
            TcpClientActor,
            TcpClientArgs {
                config: client_config.clone(),
                registration_id: None,
                delivery: cassini_client::DeliveryMode::Broadcast(events_output.clone()),
            },
        )
        .await
        {
            Ok(spawned) => break spawned,
            Err(e) => {
                tracing::warn!("failed to establish Cassini connection: {e}, retrying...");
                tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
            }
        }
    };

    let (crypto_actor, _) = Actor::spawn(
        None,
        polar_crypto::CryptoActor,
        polar_crypto::CryptoActorArgs {
            tcp_client,
            events_output,
            own_identity: own_identity.to_string(),
        },
    )
    .await
    .expect("CryptoActor::pre_start does no fallible work of its own -- spawning it can't genuinely fail here");

    loop {
        match ractor::call!(crypto_actor, |reply| {
            polar_crypto::CryptoMessage::EnsureBootstrapped { reply }
        }) {
            Ok(Ok(())) => {
                tracing::info!("escrow-keypair bootstrap complete");
                return;
            }
            Ok(Err(e)) => {
                tracing::warn!("escrow-keypair bootstrap failed: {e}, retrying...");
            }
            Err(e) => {
                tracing::warn!("failed to reach CryptoActor: {e}, retrying...");
            }
        }
        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}

// ---------------------------------------------------------------------------
// Dep cert polling
// ---------------------------------------------------------------------------

async fn wait_for_dep_certs(endpoints: &[DepCertEndpoint]) {
    tracing::info!(
        "waiting for {} dep cert endpoint(s) to be healthy",
        endpoints.len()
    );

    loop {
        let all_healthy = endpoints.iter().all(|ep| match ep.check_ttl() {
            Ok(remaining) => {
                tracing::info!(
                    "dep cert {}:{} healthy, {remaining}s remaining",
                    ep.host,
                    ep.port
                );
                true
            }
            Err(reason) => {
                tracing::warn!("dep cert not yet healthy: {reason}");
                false
            }
        });

        if all_healthy {
            tracing::info!("all dep certs healthy, proceeding to cert issuance");
            return;
        }

        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}

// ---------------------------------------------------------------------------
// Cert issuance
// ---------------------------------------------------------------------------

async fn issue_cert(
    cert_issuer_url: &str,
    sa_token: &str,
    dns_identity: &str,
    req: &CertRequest,
) {
    tracing::info!(
        "issuing {:?} cert into '{}' (algorithm: {:?}, extra-sans: {:?})",
        req.cert_type,
        req.cert_dir,
        req.key_algorithm,
        req.extra_sans,
    );

    let csr_output = loop {
        match keypair::generate_csr(dns_identity, &req.key_algorithm, &req.extra_sans) {
            Ok(c) => break c,
            Err(e) => {
                // CSR generation failure is almost certainly a bug or
                // misconfiguration — log and exit rather than looping forever.
                eprintln!("error: failed to generate CSR: {e}");
                std::process::exit(1);
            }
        }
    };

    let client = handshake::HandshakeClient::new(cert_issuer_url.to_string());

    let response = loop {
        match client
            .issue(
                sa_token,
                &csr_output.csr_pem,
                req.cert_type.clone(),
                req.extra_sans.clone(),
            )
            .await
        {
            Ok(r) => break r,
            Err(handshake::HandshakeError::Unreachable(e)) => {
                tracing::warn!("cert issuer unreachable: {e}, retrying...");
                tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
            }
            Err(handshake::HandshakeError::Rejected(e)) => {
                // Rejection is unrecoverable — misconfiguration or
                // identity mismatch. Exit so the operator can investigate.
                eprintln!(
                    "error: cert issuer rejected request: {:?} — {}",
                    e.outcome, e.detail
                );
                std::process::exit(1);
            }
            Err(handshake::HandshakeError::Malformed(e)) => {
                // Malformed response could be a transient proxy issue.
                // Retry with a warning rather than exiting.
                tracing::warn!("malformed cert issuer response: {e}, retrying...");
                tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
            }
        }
    };

    let bundle = output::OutputBundle {
        cert_pem: &response.certificate_pem,
        key_pem: &csr_output.private_key_pem,
        ca_pem: &response.ca_chain_pem,
    };

    loop {
        match output::write_bundle(std::path::Path::new(&req.cert_dir), &bundle) {
            Ok(()) => {
                tracing::info!(
                    "issued {:?} cert for '{}', expires {}",
                    req.cert_type,
                    dns_identity,
                    response.expires_at,
                );
                return;
            }
            Err(e) => {
                tracing::warn!("failed to write cert bundle to '{}': {e}, retrying...", req.cert_dir);
                tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
            }
        }
    }
}
