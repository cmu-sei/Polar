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

//! An end-to-end harness for the broker: a real `Broker` listening on a loopback port with a
//! throwaway certificate authority, and real clients that connect to it over mutual TLS.
//!
//! The broker's actors register under fixed names, so only one broker can exist in a process at a
//! time. A test takes [`serial`] first and stops its broker at the end.
#![allow(dead_code)]

use cassini_broker::broker::{Broker, BrokerArgs};
use cassini_client::facade::{
    Backoff, DEFAULT_OFFLINE_BUFFER, FacadeArgs, TcpClientFacade, TlsTransportSpawner,
};
use cassini_client::{
    DeliveryMode, TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage,
};
use cassini_types::{BrokerMessage, ClientEvent};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, async_trait};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Held for the whole of a test, so brokers never overlap.
pub async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

pub struct Pki {
    dir: tempfile::TempDir,
    issuer: Issuer<'static, KeyPair>,
    /// The broker's key: its key file, unless [`Pki::use_custody`] gave it the custody daemon's server slot.
    server_key: Option<polar_custody::client::ServerKey>,
}

impl Pki {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let key = KeyPair::generate().expect("a CA key");
        let mut params = CertificateParams::new(Vec::new()).expect("CA parameters");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let cert = params.self_signed(&key).expect("a CA certificate");
        std::fs::write(dir.path().join("ca.pem"), cert.pem()).unwrap();
        std::fs::write(dir.path().join("ca.key"), key.serialize_pem()).unwrap();
        let issuer = Issuer::new(params, key);
        let pki = Self {
            dir,
            issuer,
            server_key: None,
        };
        pki.issue("server", "localhost", ExtendedKeyUsagePurpose::ServerAuth);
        pki
    }

    fn issue(&self, name: &str, san: &str, usage: ExtendedKeyUsagePurpose) {
        let key = KeyPair::generate().expect("a key");
        let mut params = CertificateParams::new(vec![san.to_string()]).expect("parameters");
        params.extended_key_usages = vec![usage];
        let cert = params
            .signed_by(&key, &self.issuer)
            .expect("a signed certificate");
        std::fs::write(self.dir.path().join(format!("{name}.pem")), cert.pem()).unwrap();
        std::fs::write(
            self.dir.path().join(format!("{name}.key")),
            key.serialize_pem(),
        )
        .unwrap();
    }

    fn path(&self, file: &str) -> PathBuf {
        self.dir.path().join(file)
    }

    /// The broker's key from now on is the custody daemon's at `socket` (docs/architecture/key-custody.md, step 4):
    /// polar-init's part done here, the daemon's CSR for its server slot certified and installed, and the certificate
    /// written where the broker reads its own.
    pub fn use_custody(&mut self, socket: &Path) {
        use polar_custody::client::CustodyClient;
        use polar_custody::protocol::Slot;
        let daemon = CustodyClient::new(socket);
        let csr = daemon
            .make_csr(Slot::Server, "localhost", &[])
            .expect("the daemon's CSR");
        let mut request = rcgen::CertificateSigningRequestParams::from_pem(&csr).unwrap();
        request.params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = request.signed_by(&self.issuer).unwrap().pem();
        daemon
            .install(Slot::Server, vec![certificate.clone()])
            .expect("installed");
        std::fs::write(self.path("server.pem"), certificate).unwrap();
        std::fs::remove_file(self.path("server.key")).unwrap();
        self.server_key = Some(polar_custody::client::ServerKey::Custody(
            socket.display().to_string(),
        ));
    }

    fn server_key(&self) -> polar_custody::client::ServerKey {
        self.server_key.clone().unwrap_or_else(|| {
            polar_custody::client::ServerKey::File(
                self.path("server.key").to_string_lossy().into_owned(),
            )
        })
    }

    /// Files for a client whose certificate names `identity`, as the broker reads it.
    pub fn client(&self, identity: &str) -> (PathBuf, PathBuf) {
        let name = format!("client-{identity}");
        if !self.path(&format!("{name}.pem")).exists() {
            self.issue(&name, identity, ExtendedKeyUsagePurpose::ClientAuth);
        }
        (
            self.path(&format!("{name}.pem")),
            self.path(&format!("{name}.key")),
        )
    }

    pub fn ca(&self) -> PathBuf {
        self.path("ca.pem")
    }

    /// Another client certificate for `identity`, its files apart from `client`'s under `name`, as a second pod of one
    /// workload holds: the certificate and key paths, and its serial.
    pub fn another_client(&self, name: &str, identity: &str) -> (PathBuf, PathBuf, String) {
        let file = format!("other-{name}");
        self.issue(&file, identity, ExtendedKeyUsagePurpose::ClientAuth);
        let cert = self.path(&format!("{file}.pem"));
        let serial =
            cert_issuer_common::serial::serial_from_pem(&std::fs::read_to_string(&cert).unwrap())
                .expect("a certificate");
        (cert, self.path(&format!("{file}.key")), serial)
    }

    /// What cert-issuer gives the credentials agent for its authority key (`CertType::Signing`): `identity` as the DNS
    /// name, `digitalSignature` and `id-kp-documentSigning`, over `public_key_pem`, under this CA.
    pub fn certify_authority(&self, identity: &str, public_key_pem: &str) -> String {
        let mut params = CertificateParams::new(vec![identity.to_string()]).expect("parameters");
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::Other(
            cert_issuer_common::OID_DOCUMENT_SIGNING.to_vec(),
        )];
        let subject = rcgen::SubjectPublicKeyInfo::from_pem(public_key_pem).expect("a public key");
        params
            .signed_by(&subject, &self.issuer)
            .expect("a signed certificate")
            .pem()
    }

    /// What cert-issuer gives a pod's CSR: a client certificate under this CA, as polar-init has it certified.
    /// What cert-issuer signs for a custodian: `csr_pem`, a client certificate carrying `uris`.
    pub fn certify_client_csr_carrying(&self, csr_pem: &str, uris: &[String]) -> String {
        let mut request = rcgen::CertificateSigningRequestParams::from_pem(csr_pem).unwrap();
        for uri in uris {
            request
                .params
                .subject_alt_names
                .push(rcgen::SanType::URI(uri.clone().try_into().unwrap()));
        }
        request.params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        request.signed_by(&self.issuer).unwrap().pem()
    }

    pub fn certify_client_csr(&self, csr_pem: &str) -> String {
        let mut request = rcgen::CertificateSigningRequestParams::from_pem(csr_pem).unwrap();
        request.params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        request.signed_by(&self.issuer).unwrap().pem()
    }

    /// The serial of `identity`'s client certificate, written as the credentials agent's ledger writes it.
    pub fn serial_of(&self, identity: &str) -> String {
        let (cert, _) = self.client(identity);
        cert_issuer_common::serial::serial_from_pem(&std::fs::read_to_string(cert).unwrap())
            .expect("a certificate")
    }

    /// The CA's key, for a test that runs cert-issuer as this CA.
    pub fn ca_key(&self) -> PathBuf {
        self.path("ca.key")
    }

    /// Issues the client certificate for `identity` carrying `uris` as URI names besides it, as cert-issuer writes
    /// `p:` and `g:` names, and returns its size in DER. `client` and `TestClient::connect` use it from then on.
    pub fn issue_client_carrying(&self, identity: &str, uris: &[String]) -> usize {
        let name = format!("client-{identity}");
        let key = KeyPair::generate().expect("a key");
        let mut params = CertificateParams::new(vec![identity.to_string()]).expect("parameters");
        for uri in uris {
            params.subject_alt_names.push(rcgen::SanType::URI(
                uri.clone().try_into().expect("a URI name"),
            ));
        }
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let cert = params
            .signed_by(&key, &self.issuer)
            .expect("a signed certificate");
        std::fs::write(self.path(&format!("{name}.pem")), cert.pem()).unwrap();
        std::fs::write(self.path(&format!("{name}.key")), key.serialize_pem()).unwrap();
        cert.der().len()
    }
}

/// The policy a test broker starts with unless the test is about who may do what: everything is permitted, so the
/// broker's refusal by default (a certificate that names no topics may use none) does not stand in the way of tests
/// about sessions, delivery and shutdown. `TestBroker::start_enforcing` starts with none.
pub const OPEN_POLICY: &str = "permit (principal, action, resource);";

/// How long a test broker's connections keep a decision unless a test says: the broker's own default.
pub const DEFAULT_CACHE: Duration = Duration::from_secs(5);

/// What the builder made for the broker, left where a test can reach it.
type Infrastructure = Arc<Mutex<Option<polar_agent_builder::ServiceResources>>>;

pub struct TestBroker {
    pub addr: String,
    pub pki: Pki,
    broker: Mutex<ActorRef<BrokerMessage>>,
    infrastructure: Mutex<Infrastructure>,
    shutdown_token: Option<String>,
    retention_secs: Option<u64>,
    /// Whether it starts with [`OPEN_POLICY`] (again after a restart).
    open: bool,
    /// How long a connection keeps a decision.
    cache: Duration,
    /// How long a watcher holds its place; the broker's default when `None`.
    tenure: Option<Duration>,
    /// The exit codes the broker asked the process to exit with.
    pub exits: Arc<Mutex<Vec<i32>>>,
}

/// The names the broker's actors register under. A broker left over from a test that failed holds
/// them, so every start clears them first.
const NAMES: [&str; 6] = [
    cassini_broker::BROKER_NAME,
    cassini_broker::LISTENER_MANAGER_NAME,
    cassini_broker::SESSION_MANAGER_NAME,
    cassini_broker::TOPIC_MANAGER_NAME,
    cassini_broker::SUBSCRIBER_MANAGER_NAME,
    cassini_broker::CONTROL_MANAGER_NAME,
];

async fn clear_names() {
    let authority = [
        polar_agent_builder::authority::policy_actor_name(cassini_broker::authority::SERVICE),
        polar_agent_builder::authority::permissions_actor_name(cassini_broker::authority::SERVICE),
    ];
    for _ in 0..100 {
        let mut any = false;
        for name in NAMES
            .iter()
            .map(|n| n.to_string())
            .chain(authority.iter().cloned())
        {
            if let Some(left_over) = ractor::registry::where_is(name) {
                any = true;
                left_over.kill();
            }
        }
        if !any {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the broker's names never became free");
}

fn init_logging() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
    });
}

impl TestBroker {
    pub async fn start() -> Self {
        Self::start_retaining(None).await
    }

    /// As `start`, with a disconnected session kept for `retention_secs` seconds (the broker's own
    /// default, ten minutes, when `None`).
    pub async fn start_retaining(retention_secs: Option<u64>) -> Self {
        Self::start_with(retention_secs, Duration::from_secs(20)).await
    }

    /// As `start_retaining`, with `shutdown_budget` to finish an orderly shutdown in. The broker
    /// does not exit the process: it records the code it would have exited with in `exits`.
    pub async fn start_with(retention_secs: Option<u64>, shutdown_budget: Duration) -> Self {
        Self::start_guarded(retention_secs, shutdown_budget, None).await
    }

    /// As `start_with`, with a shutdown token that a request to shut down over the wire must
    /// present.
    pub async fn start_guarded(
        retention_secs: Option<u64>,
        shutdown_budget: Duration,
        shutdown_token: Option<&str>,
    ) -> Self {
        Self::start_full(
            retention_secs,
            shutdown_budget,
            shutdown_token,
            true,
            DEFAULT_CACHE,
            None,
            None,
        )
        .await
    }

    /// A broker with no policy, as on a fresh cluster: what a client's certificate admits decides, and anything
    /// else is refused.
    pub async fn start_enforcing() -> Self {
        Self::start_enforcing_caching(DEFAULT_CACHE).await
    }

    /// As `start_enforcing`, each connection keeping a decision for `cache` (none when zero).
    pub async fn start_enforcing_caching(cache: Duration) -> Self {
        Self::start_full(
            None,
            Duration::from_secs(20),
            None,
            false,
            cache,
            None,
            None,
        )
        .await
    }

    /// As `start`, with the certificates `pki` holds: the custody daemon's server key, say.
    pub async fn start_on(pki: Pki) -> Self {
        Self::start_full(
            None,
            Duration::from_secs(20),
            None,
            true,
            DEFAULT_CACHE,
            Some(pki),
            None,
        )
        .await
    }

    /// As `start`, a watcher holding its place for `tenure` (docs/architecture/cassini-broker.md, "Capping watchers").
    pub async fn start_with_tenure(tenure: Duration) -> Self {
        Self::start_full(
            None,
            Duration::from_secs(20),
            None,
            true,
            DEFAULT_CACHE,
            None,
            Some(tenure),
        )
        .await
    }

    async fn start_full(
        retention_secs: Option<u64>,
        shutdown_budget: Duration,
        shutdown_token: Option<&str>,
        open: bool,
        cache: Duration,
        pki: Option<Pki>,
        tenure: Option<Duration>,
    ) -> Self {
        init_logging();
        clear_names().await;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pki = pki.unwrap_or_else(Pki::new);
        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            probe.local_addr().unwrap().port()
        };
        let addr = format!("127.0.0.1:{port}");
        let exits = Arc::new(Mutex::new(Vec::new()));
        let token = shutdown_token.map(str::to_string);
        let (broker, infrastructure) = Self::spawn_broker(
            &addr,
            &pki,
            retention_secs,
            shutdown_budget,
            &exits,
            token.clone(),
            cache,
            tenure,
        )
        .await;

        for _ in 0..100 {
            if tokio::net::TcpStream::connect(&addr).await.is_ok() {
                let started = Self {
                    addr,
                    pki,
                    broker: Mutex::new(broker),
                    infrastructure: Mutex::new(infrastructure),
                    shutdown_token: token,
                    retention_secs,
                    open,
                    cache,
                    tenure,
                    exits,
                };
                if open {
                    started.set_policy(OPEN_POLICY).await;
                }
                return started;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the broker never began listening on {addr}");
    }

    #[allow(clippy::too_many_arguments)]
    async fn spawn_broker(
        addr: &str,
        pki: &Pki,
        retention_secs: Option<u64>,
        shutdown_budget: Duration,
        exits: &Arc<Mutex<Vec<i32>>>,
        shutdown_auth_token: Option<String>,
        decision_cache: Duration,
        watcher_tenure: Option<Duration>,
    ) -> (ActorRef<BrokerMessage>, Infrastructure) {
        // The builder's infrastructure as the broker gets it in the pod, except that the process
        // is not ended (the code it would have exited with is recorded in `exits`) and the
        // process's signals are left alone.
        let env: std::collections::HashMap<String, String> = [(
            "POLAR_HEALTH_FILE".to_string(),
            pki.path("health.json").to_string_lossy().into_owned(),
        )]
        .into_iter()
        .collect();
        let (configured, infrastructure) =
            polar_agent_builder::Configured::for_service_test(&env, shutdown_budget, exits.clone());
        let (broker, _) = Actor::spawn(
            Some(cassini_broker::BROKER_NAME.to_string()),
            Broker,
            BrokerArgs {
                bind_addr: addr.to_string(),
                session_timeout: retention_secs,
                server_cert_file: pki.path("server.pem").to_string_lossy().into_owned(),
                server_key: pki.server_key(),
                ca_cert_file: pki.ca().to_string_lossy().into_owned(),
                shutdown_auth_token,
                decision_cache,
                watcher_tenure,
                configured,
            },
        )
        .await
        .expect("the broker starts");
        (broker, infrastructure)
    }

    /// Stops the broker, then starts another on the same address with the same certificates:
    /// everything the first one held, sessions included, is gone. Returns how long the broker
    /// was away.
    pub async fn restart(&self, away_for: Duration) -> Duration {
        let began = std::time::Instant::now();
        self.stop().await;
        tokio::time::sleep(away_for).await;
        let (broker, infrastructure) = Self::spawn_broker(
            &self.addr,
            &self.pki,
            self.retention_secs,
            Duration::from_secs(20),
            &self.exits,
            self.shutdown_token.clone(),
            self.cache,
            self.tenure,
        )
        .await;
        *self.broker.lock().unwrap() = broker;
        *self.infrastructure.lock().unwrap() = infrastructure;
        if self.open {
            self.set_policy(OPEN_POLICY).await;
        }
        began.elapsed()
    }

    /// Gives the broker's policy actor `source`, as a verified update from the credentials agent would.
    pub async fn set_policy(&self, source: &str) {
        let policy =
            polar_agent_builder::PolicyLookup::for_service(cassini_broker::authority::SERVICE)
                .actor()
                .expect("the broker's policy actor is running");
        ractor::call!(policy, |reply| polar_policy::PolicyMessage::UpdatePolicy {
            policy_source: source.to_string(),
            reply,
        })
        .expect("the policy actor answers")
        .expect("the policy is taken");
    }

    /// Whether the broker still has the session `registration_id`, connected or kept for its client's return.
    pub async fn has_session(&self, registration_id: &str) -> bool {
        let sessions: ActorRef<BrokerMessage> =
            ractor::registry::where_is(cassini_broker::SESSION_MANAGER_NAME.to_string())
                .expect("the session manager is running")
                .into();
        ractor::call!(sessions, |reply_to| BrokerMessage::GetSessionRef {
            registration_id: registration_id.to_string(),
            reply_to,
        })
        .expect("the session manager answers")
        .is_some()
    }

    /// The health actor the builder gave the broker, to end it as it would end itself.
    pub fn health_actor(&self) -> ActorRef<polar_health::HealthCheckMessage> {
        let infrastructure = self.infrastructure.lock().unwrap().clone();
        let resources = infrastructure
            .lock()
            .unwrap()
            .clone()
            .expect("the broker built its infrastructure");
        resources.healthcheck.as_actor_ref()
    }

    /// The broker supervisor, to send it what the running broker would be sent.
    pub fn actor(&self) -> ActorRef<BrokerMessage> {
        self.broker.lock().unwrap().clone()
    }

    /// Waits for the broker to end by itself, and says whether it did within `within`.
    pub async fn ends_by_itself(&self, within: Duration) -> bool {
        let broker = self.actor();
        let deadline = tokio::time::Instant::now() + within;
        while tokio::time::Instant::now() < deadline {
            if matches!(
                broker.get_status(),
                ractor::ActorStatus::Stopping | ractor::ActorStatus::Stopped
            ) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    /// Stops the broker and waits until its names are free for the next test.
    pub async fn stop(&self) {
        let broker = self.broker.lock().unwrap().clone();
        broker
            .stop_and_wait(None, Some(Duration::from_secs(10)))
            .await
            .ok();
        clear_names().await;
    }
}

/// As `with_broker`, with a shutdown token a request to shut down over the wire must present.
pub async fn with_broker_guarded<F, Fut>(token: &str, test: F)
where
    F: FnOnce(Arc<TestBroker>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let _only = serial().await;
    let broker =
        Arc::new(TestBroker::start_guarded(None, Duration::from_secs(20), Some(token)).await);
    let outcome = tokio::spawn(test(broker.clone())).await;
    broker.stop().await;
    if let Err(failure) = outcome {
        std::panic::resume_unwind(failure.into_panic());
    }
}

/// As `with_broker`, with `shutdown_budget` to finish an orderly shutdown in.
pub async fn with_broker_shutting_down_within<F, Fut>(shutdown_budget: Duration, test: F)
where
    F: FnOnce(Arc<TestBroker>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let _only = serial().await;
    let broker = Arc::new(TestBroker::start_with(None, shutdown_budget).await);
    let outcome = tokio::spawn(test(broker.clone())).await;
    broker.stop().await;
    if let Err(failure) = outcome {
        std::panic::resume_unwind(failure.into_panic());
    }
}

/// Runs `test` against a fresh broker, and stops the broker whether the test passes or panics, so
/// one failure does not leave names registered and fail every test after it.
pub async fn with_broker<F, Fut>(test: F)
where
    F: FnOnce(Arc<TestBroker>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    with_broker_retaining(None, test).await
}

/// As `with_broker`, with a disconnected session kept for `retention_secs` seconds.
pub async fn with_broker_retaining<F, Fut>(retention_secs: Option<u64>, test: F)
where
    F: FnOnce(Arc<TestBroker>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let _only = serial().await;
    let broker = Arc::new(TestBroker::start_retaining(retention_secs).await);
    let outcome = tokio::spawn(test(broker.clone())).await;
    broker.stop().await;
    if let Err(failure) = outcome {
        std::panic::resume_unwind(failure.into_panic());
    }
}

/// Collects the events a client's transport reports.
struct Collector {
    events: Arc<Mutex<Vec<ClientEvent>>>,
}

#[async_trait]
impl Actor for Collector {
    type Msg = ClientEvent;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        event: Self::Msg,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }
}

/// A real Cassini client, the bare transport, connected to the test broker.
pub struct TestClient {
    pub transport: ActorRef<TcpClientMessage>,
    events: Arc<Mutex<Vec<ClientEvent>>>,
}

impl TestClient {
    /// Connects as `identity` and waits until the broker has registered it.
    pub async fn connect(broker: &TestBroker, identity: &str) -> Self {
        Self::connect_resuming(broker, identity, None).await
    }

    /// As `connect`, asking to resume `registration_id`.
    pub async fn connect_resuming(
        broker: &TestBroker,
        identity: &str,
        registration_id: Option<String>,
    ) -> Self {
        let client = Self::start_connecting(broker, identity, registration_id, false).await;
        client
            .wait_for("a registration", Duration::from_secs(10), |c| {
                c.registration_id().is_some()
            })
            .await;
        client
    }

    /// Starts a watcher under the certificate and key at these paths, and returns at once. Like the custody daemon's,
    /// it does not reconnect by itself: one replaced or refused stays gone.
    pub async fn start_watching_with(broker: &TestBroker, cert: &Path, key: &Path) -> Self {
        let config = TCPClientConfig::from_values(
            broker.addr.clone(),
            "localhost".to_string(),
            broker.pki.ca().to_string_lossy().into_owned(),
            cert.to_string_lossy().into_owned(),
            key.to_string_lossy().into_owned(),
        )
        .as_watcher()
        .with_reconnect(cassini_client::ReconnectMode::LeaveToSupervisor);
        Self::spawn(config, None).await
    }

    /// Connects as `identity`'s watcher session (docs/architecture/key-custody.md, step 5), as the custody daemon
    /// does, and waits until the broker has registered it.
    pub async fn connect_watching(broker: &TestBroker, identity: &str) -> Self {
        let client = Self::start_connecting(broker, identity, None, true).await;
        client
            .wait_for("a watcher's registration", Duration::from_secs(10), |c| {
                c.registration_id().is_some()
            })
            .await;
        client
    }

    /// Connects with `config` as given (a key of the custody daemon's, say) and waits until the broker has registered it.
    pub async fn connect_configured(config: TCPClientConfig) -> Self {
        let client = Self::spawn(config, None).await;
        client
            .wait_for("a registration", Duration::from_secs(10), |c| {
                c.registration_id().is_some()
            })
            .await;
        client
    }

    /// Starts connecting as `identity`, a watcher or not, and returns at once: for a client the broker may turn away.
    pub async fn start_connecting(
        broker: &TestBroker,
        identity: &str,
        registration_id: Option<String>,
        watcher: bool,
    ) -> Self {
        let (cert, key) = broker.pki.client(identity);
        let config = TCPClientConfig::from_values(
            broker.addr.clone(),
            "localhost".to_string(),
            broker.pki.ca().to_string_lossy().into_owned(),
            cert.to_string_lossy().into_owned(),
            key.to_string_lossy().into_owned(),
        );
        // A watcher does not reconnect by itself, like the custody daemon's: one replaced or refused stays gone.
        let config = if watcher {
            config
                .as_watcher()
                .with_reconnect(cassini_client::ReconnectMode::LeaveToSupervisor)
        } else {
            config
        };
        Self::spawn(config, registration_id).await
    }

    async fn spawn(config: TCPClientConfig, registration_id: Option<String>) -> Self {
        let events = Arc::new(Mutex::new(Vec::new()));
        let port = Arc::new(OutputPort::default());
        let (collector, _) = Actor::spawn(
            None,
            Collector {
                events: events.clone(),
            },
            (),
        )
        .await
        .unwrap();
        port.subscribe(collector, Some);
        let (transport, _) = Actor::spawn(
            None,
            TcpClientActor,
            TcpClientArgs {
                config,
                registration_id,
                delivery: DeliveryMode::Broadcast(port),
                answers: None,
            },
        )
        .await
        .expect("the client starts");
        Self { transport, events }
    }

    /// The connection to the broker was lost, as the client itself reports it.
    pub fn lost_its_connection(&self) -> bool {
        self.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, ClientEvent::TransportError { .. }))
    }

    pub fn registration_events(&self) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, ClientEvent::Registered { .. }))
            .count()
    }

    /// The id of the session the broker gave this client, once it has.
    pub fn registration_id(&self) -> Option<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|e| match e {
                ClientEvent::Registered { registration_id } => Some(registration_id.clone()),
                _ => None,
            })
    }

    pub fn subscribe(&self, topic: &str) {
        self.transport
            .cast(TcpClientMessage::Subscribe {
                topic: topic.to_string(),
                trace_ctx: None,
            })
            .unwrap();
    }

    pub fn publish(&self, topic: &str, payload: &[u8]) {
        self.transport
            .cast(TcpClientMessage::publish(
                topic.to_string(),
                payload.to_vec(),
                None,
            ))
            .unwrap();
    }

    /// Asks the broker to do something over the wire, as the control tool does.
    pub fn control(&self, op: cassini_types::ControlOp) {
        self.transport
            .cast(TcpClientMessage::ControlRequest {
                op,
                trace_ctx: None,
            })
            .unwrap();
    }

    /// The answers the broker has given to control requests, in order.
    pub fn control_results(
        &self,
    ) -> Vec<Result<cassini_types::ControlResult, cassini_types::ControlError>> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                ClientEvent::ControlResponse { result, .. } => Some(result.clone()),
                _ => None,
            })
            .collect()
    }

    /// An explicit goodbye.
    pub fn say_goodbye(&self) {
        self.transport
            .cast(TcpClientMessage::Disconnect { trace_ctx: None })
            .unwrap();
    }

    /// The process dies without a word: the connection closes, nothing is said first. (The
    /// transport is stopped, not killed: a kill skips its `post_stop`, which is what aborts the
    /// reader task that still holds the socket, so the connection would stay open.)
    pub async fn vanish(&self) {
        self.transport
            .stop_and_wait(None, Some(Duration::from_secs(5)))
            .await
            .ok();
    }

    /// Payloads delivered on `topic` so far, in order, each with who the broker says published it.
    pub fn received_from(&self, topic: &str) -> Vec<(Vec<u8>, Option<String>)> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                ClientEvent::MessagePublished {
                    topic: t,
                    payload,
                    published_by,
                    ..
                } if t == topic => Some((payload.clone(), published_by.clone())),
                _ => None,
            })
            .collect()
    }

    /// Payloads delivered on `topic` so far, in order.
    pub fn received(&self, topic: &str) -> Vec<Vec<u8>> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                ClientEvent::MessagePublished {
                    topic: t, payload, ..
                } if t == topic => Some(payload.to_vec()),
                _ => None,
            })
            .collect()
    }

    /// How many messages have been delivered on `topic`, without copying them (as `received` does).
    pub fn received_count(&self, topic: &str) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, ClientEvent::MessagePublished { topic: t, .. } if t == topic))
            .count()
    }

    /// How many publishes on `topic` the broker has acknowledged.
    pub fn acknowledged(&self, topic: &str) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, ClientEvent::PublishAcknowledged { topic: t } if t == topic))
            .count()
    }

    /// Whether the broker has acknowledged a subscription to `topic`.
    pub fn subscribed(&self, topic: &str) -> bool {
        self.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, ClientEvent::ClientSubscribed { topic: t } if t == topic))
    }

    /// The reasons the broker gave for refusing a subscription to, or a publish on, `topic`.
    pub fn refusals(&self, topic: &str) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                ClientEvent::SubscribeRefused { topic: t, reason }
                | ClientEvent::PublishRefused { topic: t, reason }
                    if t == topic =>
                {
                    Some(reason.clone())
                }
                _ => None,
            })
            .collect()
    }

    pub async fn wait_for(&self, what: &str, within: Duration, ready: impl Fn(&Self) -> bool) {
        let deadline = tokio::time::Instant::now() + within;
        while tokio::time::Instant::now() < deadline {
            if ready(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for {what}");
    }

    pub async fn wait_for_messages(&self, topic: &str, count: usize) -> Vec<Vec<u8>> {
        self.wait_for(
            &format!("{count} message(s) on {topic}"),
            Duration::from_secs(10),
            |c| c.received(topic).len() >= count,
        )
        .await;
        self.received(topic)
    }
}

/// A client held by the stable front the agents use, which brings its connection back by itself.
pub struct FacadeClient {
    pub client: TestClient,
    transport_name: String,
    online: Arc<std::sync::atomic::AtomicBool>,
}

impl FacadeClient {
    pub async fn connect(broker: &TestBroker, identity: &str) -> Self {
        let (cert, key) = broker.pki.client(identity);
        let events = Arc::new(Mutex::new(Vec::new()));
        let port = Arc::new(OutputPort::default());
        let (collector, _) = Actor::spawn(
            None,
            Collector {
                events: events.clone(),
            },
            (),
        )
        .await
        .unwrap();
        port.subscribe(collector, Some);
        let transport_name = format!("transport-{}", uuid::Uuid::new_v4());
        let online = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (facade, _) = Actor::spawn(
            None,
            TcpClientFacade,
            FacadeArgs {
                config: TCPClientConfig::from_values(
                    broker.addr.clone(),
                    "localhost".to_string(),
                    broker.pki.ca().to_string_lossy().into_owned(),
                    cert.to_string_lossy().into_owned(),
                    key.to_string_lossy().into_owned(),
                ),
                delivery: DeliveryMode::Broadcast(port),
                transport_name: transport_name.clone(),
                spawner: Arc::new(TlsTransportSpawner),
                online: online.clone(),
                gate: None,
                watch: None,
                buffer_limit: DEFAULT_OFFLINE_BUFFER,
                backoff: Backoff {
                    first: Duration::from_millis(200),
                    last: Duration::from_secs(1),
                },
            },
        )
        .await
        .expect("the client starts");
        let client = TestClient {
            transport: facade,
            events,
        };
        client
            .wait_for("a registration", Duration::from_secs(10), |c| {
                c.registration_id().is_some()
            })
            .await;
        Self {
            client,
            transport_name,
            online,
        }
    }

    /// Whether the front believes a transport is registered with the broker.
    pub fn is_online(&self) -> bool {
        self.online.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The actor id of the transport now running, to tell one from its replacement.
    pub fn transport_id(&self) -> Option<u64> {
        ractor::registry::where_is(self.transport_name.clone()).map(|cell| cell.get_id().pid())
    }

    /// The connection drops (the transport stops and its socket closes); the front stays up.
    pub async fn lose_the_connection(&self) {
        let transport = ractor::registry::where_is(self.transport_name.clone())
            .expect("a transport is running");
        ActorRef::<TcpClientMessage>::from(transport)
            .stop_and_wait(None, Some(Duration::from_secs(5)))
            .await
            .ok();
    }

    /// How many times the broker has registered this client, counting a resume.
    pub fn registrations(&self) -> Vec<String> {
        self.client
            .events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                ClientEvent::Registered { registration_id } => Some(registration_id.clone()),
                _ => None,
            })
            .collect()
    }
}

pub fn pause() -> impl std::future::Future<Output = ()> {
    tokio::time::sleep(Duration::from_millis(300))
}

pub fn exists(path: &Path) -> bool {
    path.exists()
}
