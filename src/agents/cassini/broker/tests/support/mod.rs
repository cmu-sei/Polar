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
        let issuer = Issuer::new(params, key);
        let pki = Self { dir, issuer };
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
}

pub struct TestBroker {
    pub addr: String,
    pub pki: Pki,
    broker: ActorRef<BrokerMessage>,
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
    for _ in 0..100 {
        let mut any = false;
        for name in NAMES {
            if let Some(left_over) = ractor::registry::where_is(name.to_string()) {
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
        init_logging();
        clear_names().await;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pki = Pki::new();
        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            probe.local_addr().unwrap().port()
        };
        let addr = format!("127.0.0.1:{port}");
        let (broker, _) = Actor::spawn(
            Some(cassini_broker::BROKER_NAME.to_string()),
            Broker,
            BrokerArgs {
                bind_addr: addr.clone(),
                session_timeout: retention_secs,
                server_cert_file: pki.path("server.pem").to_string_lossy().into_owned(),
                private_key_file: pki.path("server.key").to_string_lossy().into_owned(),
                ca_cert_file: pki.ca().to_string_lossy().into_owned(),
                shutdown_auth_token: None,
            },
        )
        .await
        .expect("the broker starts");

        for _ in 0..100 {
            if tokio::net::TcpStream::connect(&addr).await.is_ok() {
                return Self { addr, pki, broker };
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the broker never began listening on {addr}");
    }

    /// Stops the broker and waits until its names are free for the next test.
    pub async fn stop(&self) {
        self.broker
            .stop_and_wait(None, Some(Duration::from_secs(10)))
            .await
            .ok();
        clear_names().await;
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
        let (transport, _) = Actor::spawn(
            None,
            TcpClientActor,
            TcpClientArgs {
                config: TCPClientConfig::from_values(
                    broker.addr.clone(),
                    "localhost".to_string(),
                    broker.pki.ca().to_string_lossy().into_owned(),
                    cert.to_string_lossy().into_owned(),
                    key.to_string_lossy().into_owned(),
                ),
                registration_id,
                delivery: DeliveryMode::Broadcast(port),
            },
        )
        .await
        .expect("the client starts");
        let client = Self { transport, events };
        client
            .wait_for("a registration", Duration::from_secs(10), |c| {
                c.registration_id().is_some()
            })
            .await;
        client
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
                online: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        }
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
