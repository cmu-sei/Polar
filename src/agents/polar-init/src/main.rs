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
//! Environment variables, each declared in `environment.rs` (run it against a bad one to see
//! them all named at once, before anything waits):
//!   POLAR_INIT_CERT_ISSUER_URL   URL of the cert issuer service (required)
//!   POLAR_INIT_TOKEN_PATH        Path to projected SA token
//!                                (default: /workspace/token)
//!   POLAR_INIT_CA_CERT           Path to the Polar CA certificate (PEM),
//!                                verified against from the first request.
//!                                If unset, the CA is fetched once from
//!                                GET /ca without verification and pinned.
//!   POLAR_INIT_SKIP_ESCROW_BOOTSTRAP
//!                                Any non-empty value skips the escrow bootstrap
//!   TLS_*, BROKER_ADDR, CASSINI_SERVER_NAME, KUBERNETES_SA_TOKEN
//!                                The Cassini client's and the crypto actor's,
//!                                required unless the bootstrap is skipped

// Reading the process environment goes through polar-env (see clippy.toml).
#![deny(clippy::disallowed_methods)]

mod environment;
mod goodbye;
mod handle;

use std::str::FromStr;
use std::time::Duration;

use cassini_client::{TcpClientActor, TcpClientArgs};
use cert_client::{handshake, output, token};
use cert_issuer_common::keypair;
use cert_issuer_common::{
    CertType,
    identity::{IdentityNormalization, normalize_identity},
};
use clap::Parser;
use credentials_agent_common::issuance::WorkloadCertType;
use polar_health::DepCertEndpoint;
use ractor::Actor;

const POLL_INTERVAL_SECS: u64 = 5;

/// How long the escrow-keypair bootstrap may take once the certificates are issued: its own Cassini client registering,
/// then the credentials agent answering. Neither wait ends on its own, so a client that never registers (or has died)
/// or an answer that never comes would hold the pod in its init container for good, with nothing to retry it. Past this
/// it exits 1 and the pod is replaced (2026-10-21: a load-harness pod waited eleven minutes, after its certificate was
/// issued, for an escrow request that never reached the credentials agent).
const ESCROW_BOOTSTRAP_DEADLINE: Duration = Duration::from_secs(120);

/// `work`, or why it did not finish within `deadline`.
async fn within<T>(
    deadline: Duration,
    what: &str,
    work: impl std::future::Future<Output = T>,
) -> Result<T, String> {
    tokio::time::timeout(deadline, work)
        .await
        .map_err(|_| format!("{what} did not finish within {deadline:?}"))
}

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
                ));
            }
        };

        let cert_dir = parts[1].trim().to_string();
        if cert_dir.is_empty() {
            return Err(format!(
                "invalid --cert '{}': cert_dir must not be empty",
                s
            ));
        }

        let key_algorithm = match parts[2].trim().to_lowercase().as_str() {
            "ed25519" => keypair::KeyAlgorithm::Ed25519,
            "ecdsa-p256" => keypair::KeyAlgorithm::EcdsaP256,
            other => {
                return Err(format!(
                    "invalid key algorithm '{}': expected 'ed25519' or 'ecdsa-p256'",
                    other
                ));
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

    // The environment is read once and judged as a whole before the first wait, so a mistake
    // in it is one clear exit naming every problem, never a hang or a late failure.
    let settings =
        match environment::InitSettings::from_env(&environment::snapshot(&polar_env::ProcessEnv)) {
            Ok(settings) => settings,
            Err(problems) => {
                eprintln!("error: this container's environment is not sound: {problems}");
                std::process::exit(1);
            }
        };
    let token_path = settings.token_path.clone();

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

    // Phase 3: issue all requested certs, from where this workload gets them.
    match &settings.issuance {
        environment::Issuance::CertIssuer { url, ca_cert_path } => {
            // Straight from cert-issuer, with the pod's token: the bootstrap services only (Cassini, the
            // credentials agent and its observer), which cannot wait on the credentials agent because they
            // are what it needs to run.
            //
            // The trust decision is made once, here. With POLAR_INIT_CA_CERT set the
            // client verifies cert-issuer against that file from its first request;
            // without it, the CA is fetched once and pinned (the degraded mode).
            let client = loop {
                match handshake::HandshakeClient::for_trust_mode(
                    url.clone(),
                    ca_cert_path.as_deref(),
                )
                .await
                {
                    Ok(c) => break c,
                    // A configured but unreadable or malformed CA needs an
                    // operator, and retrying can't fix it.
                    Err(e) if ca_cert_path.is_some() => {
                        eprintln!("error: cannot establish trust in cert-issuer: {e}");
                        std::process::exit(1);
                    }
                    // Fetch mode: cert-issuer not being up yet is transient, and
                    // this binary never exits non-zero for transient failures.
                    Err(e) => {
                        tracing::warn!("cannot fetch the CA from cert-issuer: {e}, retrying...");
                        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
                    }
                }
            };
            for req in &cert_requests {
                issue_cert(&client, &sa_token, &dns_identity, req).await;
            }
        }
        environment::Issuance::Observer { url } => {
            // From the credentials agent, through its observer, with the token sealed to an ephemeral
            // session so it is not on the pod network in the clear. Every other workload does this.
            let observer = credentials_agent_client::ObserverClient::new(url.clone());
            for req in &cert_requests {
                issue_cert_via_observer(&observer, &sa_token, &dns_identity, req).await;
            }
        }
    }

    tracing::info!("all certs issued");

    // Phase 4: ensure this identity's own escrow keypair genuinely
    // exists and is escrowed with the Credentials Agent -- skipped
    // entirely for the three core services (see
    // environment::SKIP_ESCROW_BOOTSTRAP's own doc comment for why). This is
    // the only point in this agent's entire lifecycle where the SA
    // token read above is ever readable at all; the main, long-
    // running agent container never has it mounted.
    match &settings.escrow {
        None => tracing::info!(
            "{} is set, skipping escrow-keypair bootstrap",
            environment::SKIP_ESCROW_BOOTSTRAP.name()
        ),
        Some(escrow) => {
            if let Err(why) = within(
                ESCROW_BOOTSTRAP_DEADLINE,
                "the escrow-keypair bootstrap",
                ensure_escrow_bootstrapped(&dns_identity, escrow),
            )
            .await
            {
                eprintln!(
                    "error: {why} (its Cassini client never registered, or the credentials agent never answered); \
                     exiting so the pod is replaced"
                );
                std::process::exit(1);
            }
        }
    }

    tracing::info!("polar-init complete, handing off to main container");
}

/// Establishes a real, standalone Cassini connection (using the
/// just-issued client-type mTLS cert), a real, bare RouterActor, and
/// a real CryptoActor instance registered against it, sends
/// EnsureBootstrapped, and retries until it succeeds -- matching this
/// binary's own, established "never exit non-zero due to a transient
/// failure" philosophy, since this genuinely depends on the
/// Credentials Agent specifically being reachable, not just
/// cert-issuer.
///
/// Constructs TcpClientActor with DeliveryMode::Router directly
/// (mirroring lib::cassini::TcpClient::spawn_with_router's own,
/// real, internal shape) rather than calling that helper itself,
/// since this binary has no real, enclosing supervisor actor to
/// link against at all -- just a bare main(). The lifecycle closure
/// is a genuine no-op: this flow never observes a ClientEvent
/// lifecycle event (Registered, TransportError, etc.) at all, only
/// CryptoActor's own EnsureBootstrapped reply.
async fn ensure_escrow_bootstrapped(own_identity: &str, escrow: &environment::EscrowSettings) {
    // The client's coordinates (TLS_CA_CERT, TLS_CLIENT_CERT, TLS_CLIENT_KEY, BROKER_ADDR and
    // CASSINI_SERVER_NAME, the same commonClientEnv every agent's main container gets) were
    // read and judged with the rest of the environment at startup, so they are certainly here.
    let client_config = escrow.client.clone();

    let (router, _) = Actor::spawn(None, polar_router::RouterActor, ())
        .await
        .expect("RouterActor::pre_start does no fallible work of its own -- spawning it can't genuinely fail here");

    let lifecycle: std::sync::Arc<dyn Fn(cassini_types::ClientEvent) + Send + Sync> =
        std::sync::Arc::new(|_event| {});

    let (tcp_client, _) = loop {
        match Actor::spawn(
            None,
            TcpClientActor,
            TcpClientArgs {
                config: client_config.clone(),
                registration_id: None,
                delivery: cassini_client::DeliveryMode::Router {
                    router: router.clone(),
                    lifecycle: lifecycle.clone(),
                },
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

    // This actor is never stopped: stopping it releases the key it holds from the kernel keyring,
    // and the key has to outlive this container for the workload to adopt it. The process exits.
    let (crypto_actor, _) = Actor::spawn(
        None,
        polar_crypto::CryptoActor,
        polar_crypto::CryptoActorArgs::new(tcp_client.clone(), router)
            .with_settings(escrow.crypto.clone()),
    )
    .await
    .expect("CryptoActor::pre_start does no fallible work of its own -- spawning it can't genuinely fail here");

    // The client drops any Subscribe sent before it has registered, so
    // wait for a registration id before telling CryptoActor to subscribe.
    // This binary has no supervisor to forward ClientEvent::Registered,
    // and its lifecycle closure above is a no-op.
    let mut polls: u64 = 0;
    loop {
        match ractor::call!(
            tcp_client,
            cassini_client::TcpClientMessage::GetRegistrationId
        ) {
            Ok(Some(_)) => break,
            Ok(None) => {}
            // A client that has died answers nothing, ever: say so, and let the deadline end the wait.
            Err(e) => {
                if polls.is_multiple_of(50) {
                    tracing::warn!("the Cassini client does not answer: {e}");
                }
            }
        }
        polls += 1;
        if polls.is_multiple_of(50) {
            tracing::info!(
                "still waiting for the Cassini client to register ({}s)",
                polls / 5
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if let Err(e) = crypto_actor.send_message(polar_crypto::CryptoMessage::ClientRegistered) {
        tracing::warn!("failed to tell CryptoActor the client registered: {e}");
    }

    loop {
        match ractor::call!(crypto_actor, |reply| {
            polar_crypto::CryptoMessage::EnsureBootstrapped { reply }
        }) {
            Ok(Ok(custody)) => {
                tracing::info!("escrow-keypair bootstrap complete ({custody:?})");
                // Tell the workload how the key is held: its number if the kernel holds it,
                // nothing if this process had to, so the workload does what it always did.
                if let Err(e) =
                    handle::record_custody(&escrow.crypto.escrow_key_handle_path, custody)
                {
                    // Not fatal: without a number the workload retrieves the key itself.
                    tracing::warn!("{e}; the workload will hold the key itself");
                }
                // Leave the broker properly so it ends this session now, rather than keeping it
                // for the workload's identity to find. The crypto actor is left running: the key
                // it holds must outlive init.
                if !goodbye::say_goodbye(&tcp_client, std::time::Duration::from_secs(2)).await {
                    tracing::warn!("could not confirm the goodbye to the broker");
                }
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
    client: &handshake::HandshakeClient,
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

    let response = loop {
        match client
            .issue(
                sa_token,
                &csr_output.csr_pem,
                req.cert_type.clone(),
                req.extra_sans.clone(),
                None,
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

    write_bundle_patiently(
        req,
        dns_identity,
        &response.certificate_pem,
        &csr_output.private_key_pem,
        &response.ca_chain_pem,
        &response.expires_at.to_string(),
    )
    .await;
}

/// Writes the certificate, its key and the CA into the requested directory, and keeps trying until it can: a
/// volume that is not writable yet is transient here, like everything but a refusal.
async fn write_bundle_patiently(
    req: &CertRequest,
    dns_identity: &str,
    cert_pem: &str,
    key_pem: &str,
    ca_pem: &str,
    expires_at: &str,
) {
    let bundle = output::OutputBundle {
        cert_pem,
        key_pem,
        ca_pem,
    };

    loop {
        match output::write_bundle(std::path::Path::new(&req.cert_dir), &bundle) {
            Ok(()) => {
                tracing::info!(
                    "issued {:?} cert for '{}', expires {}",
                    req.cert_type,
                    dns_identity,
                    expires_at,
                );
                return;
            }
            Err(e) => {
                tracing::warn!(
                    "failed to write cert bundle to '{}': {e}, retrying...",
                    req.cert_dir
                );
                tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
            }
        }
    }
}

/// The certificate type the credentials agent knows for a `--cert` type. The escrow identity is not one: it is
/// not requested here at all.
fn workload_cert_type(cert_type: &CertType) -> Option<WorkloadCertType> {
    match cert_type {
        CertType::Client => Some(WorkloadCertType::Client),
        CertType::Server => Some(WorkloadCertType::Server),
        CertType::Service => None,
    }
}

/// One certificate from the credentials agent through its observer, waiting out whatever is only unavailable and
/// stopping at what is final. `Ok` is the certificate; `Err` says why it will not come.
async fn obtain_via_observer(
    observer: &credentials_agent_client::ObserverClient,
    token: &str,
    dns_identity: &str,
    request: &polar_crypto::workload_certificates::CertificateRequest,
    delay: Duration,
) -> Result<polar_crypto::workload_certificates::IssuedCertificate, String> {
    use polar_crypto::workload_certificates::{RequestError, request_certificate};
    loop {
        match request_certificate(observer, token, dns_identity, request).await {
            Ok(issued) => return Ok(issued),
            Err(RequestError::Retry(why)) => {
                tracing::warn!("the credentials agent cannot issue just yet: {why}, retrying...");
                tokio::time::sleep(delay).await;
            }
            // A refusal is final: misconfiguration, an identity mismatch, or a pod that was already issued
            // this certificate. The pod is replaced (a new pod has a new instance binding), so exit and let
            // the operator look.
            Err(RequestError::Refused(reason)) => {
                return Err(format!(
                    "the credentials agent refused the request: {reason:?}"
                ));
            }
            Err(RequestError::Failed(why)) => return Err(why),
        }
    }
}

async fn issue_cert_via_observer(
    observer: &credentials_agent_client::ObserverClient,
    sa_token: &str,
    dns_identity: &str,
    req: &CertRequest,
) {
    tracing::info!(
        "requesting {:?} cert into '{}' from the credentials agent (algorithm: {:?}, names: {:?})",
        req.cert_type,
        req.cert_dir,
        req.key_algorithm,
        req.extra_sans,
    );
    let Some(cert_type) = workload_cert_type(&req.cert_type) else {
        eprintln!(
            "error: the credentials agent does not issue a {:?} cert here",
            req.cert_type
        );
        std::process::exit(1);
    };
    let request = polar_crypto::workload_certificates::CertificateRequest {
        cert_type,
        key_algorithm: req.key_algorithm.clone(),
        names: req.extra_sans.clone(),
    };
    match obtain_via_observer(
        observer,
        sa_token,
        dns_identity,
        &request,
        Duration::from_secs(POLL_INTERVAL_SECS),
    )
    .await
    {
        Ok(issued) => {
            write_bundle_patiently(
                req,
                dns_identity,
                &issued.certificate_pem,
                &issued.private_key_pem,
                &issued.ca_chain_pem,
                "(per the credentials agent)",
            )
            .await;
        }
        Err(why) => {
            eprintln!("error: {why}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use credentials_agent_common::ephemeral_exchange::EphemeralSession;
    use credentials_agent_common::issuance::{IssuanceRefusalReason, IssuanceReply};
    use credentials_agent_common::workload_issuance::SessionOffer;
    use polar_crypto::workload_certificates::CertificateRequest;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn a_cert_argument_maps_to_the_type_the_credentials_agent_knows_and_the_escrow_identity_does_not()
     {
        assert_eq!(
            workload_cert_type(&CertType::Client),
            Some(WorkloadCertType::Client)
        );
        assert_eq!(
            workload_cert_type(&CertType::Server),
            Some(WorkloadCertType::Server)
        );
        assert_eq!(workload_cert_type(&CertType::Service), None);
    }

    fn a_request() -> CertificateRequest {
        CertificateRequest {
            cert_type: WorkloadCertType::Client,
            key_algorithm: keypair::KeyAlgorithm::EcdsaP256,
            names: vec![],
        }
    }

    fn an_offer() -> SessionOffer {
        SessionOffer {
            session_id: "s1".to_string(),
            public_key: EphemeralSession::open().public_key(),
            valid_for_secs: 60,
        }
    }

    fn issued() -> IssuanceReply {
        IssuanceReply::Issued {
            certificate_pem: "CERT".to_string(),
            ca_chain_pem: "CHAIN".to_string(),
            escrow_public_key_pem: "ESCROW".to_string(),
        }
    }

    const MILLISECOND: Duration = Duration::from_millis(1);

    #[tokio::test]
    async fn a_wait_that_never_ends_is_ended_at_its_deadline_and_says_what_it_was() {
        let said = within(
            10 * MILLISECOND,
            "the escrow-keypair bootstrap",
            std::future::pending::<()>(),
        )
        .await
        .unwrap_err();
        assert!(
            said.contains("the escrow-keypair bootstrap did not finish within"),
            "{said}"
        );
        assert_eq!(within(10 * MILLISECOND, "x", async { 7 }).await, Ok(7));
    }

    #[tokio::test]
    async fn a_flooded_observer_is_waited_out_and_the_certificate_then_comes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/workload/session"))
            .respond_with(ResponseTemplate::new(503).set_body_string("too many sessions are open"))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/workload/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(an_offer()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/workload/issue"))
            .respond_with(ResponseTemplate::new(200).set_body_json(issued()))
            .mount(&server)
            .await;
        let observer = credentials_agent_client::ObserverClient::new(server.uri());
        let got = obtain_via_observer(&observer, "token", "a.b", &a_request(), MILLISECOND)
            .await
            .expect("it comes in the end");
        assert_eq!(got.certificate_pem, "CERT");
        let asked_for_a_session = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/workload/session")
            .count();
        assert_eq!(
            asked_for_a_session, 3,
            "two refusals to wait out, then the one that worked"
        );
    }

    #[tokio::test]
    async fn a_refusal_is_final_and_is_not_asked_again() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/workload/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(an_offer()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/workload/issue"))
            .respond_with(
                ResponseTemplate::new(403).set_body_json(IssuanceReply::Refused {
                    reason: IssuanceRefusalReason::AlreadyHasValidCertificate,
                }),
            )
            .mount(&server)
            .await;
        let observer = credentials_agent_client::ObserverClient::new(server.uri());
        let err =
            match obtain_via_observer(&observer, "token", "a.b", &a_request(), MILLISECOND).await {
                Ok(_) => panic!("a refusal must not become a certificate"),
                Err(why) => why,
            };
        assert!(err.contains("AlreadyHasValidCertificate"), "{err}");
        let attempts = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/workload/issue")
            .count();
        assert_eq!(attempts, 1);
    }
}
