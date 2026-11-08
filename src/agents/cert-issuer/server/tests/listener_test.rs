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

//! The listener actor (docs/architecture/cert-issuer-isolation.md, "1b in detail"): it serves until it is drained, a
//! drain lets the requests in flight finish within its timeout, and serving that ends any other way fails the actor,
//! so its supervisor sees it as it sees any child.

use cert_issuer::ca::{RcgenCaClient, load_or_bootstrap_ca};
use cert_issuer::listener::{Listener, ListenerArgs, ListenerMessage};
use cert_issuer::tls::build_server_config;
use ractor::{Actor, ActorProcessingErr, ActorRef, ActorStatus, SupervisionEvent};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;

const SERVER_IDENTITY: &str = "cert-issuer.test";

/// What a parent was told of its child's ending.
#[derive(Debug, Clone, PartialEq)]
enum Ending {
    Terminated,
    Failed(String),
}

struct Parent;

#[ractor::async_trait]
impl Actor for Parent {
    type Msg = ();
    type State = Arc<Mutex<Vec<Ending>>>;
    type Arguments = Arc<Mutex<Vec<Ending>>>;

    async fn pre_start(
        &self,
        _myself: ActorRef<()>,
        seen: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(seen)
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<()>,
        event: SupervisionEvent,
        seen: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorTerminated(..) => seen.lock().unwrap().push(Ending::Terminated),
            SupervisionEvent::ActorFailed(_, e) => {
                seen.lock().unwrap().push(Ending::Failed(e.to_string()))
            }
            _ => {}
        }
        Ok(())
    }
}

struct Fixture {
    addr: SocketAddr,
    ca_cert_pem: String,
    listener: ActorRef<ListenerMessage>,
    seen: Arc<Mutex<Vec<Ending>>>,
}

/// A router with `/ping`, answered at once, and `/slow`, answered after `slow`.
fn app(slow: Duration) -> axum::Router {
    axum::Router::new()
        .route("/ping", axum::routing::get(|| async { "pong" }))
        .route(
            "/slow",
            axum::routing::get(move || async move {
                tokio::time::sleep(slow).await;
                "done"
            }),
        )
}

async fn start(slow: Duration, drain_timeout: Duration) -> Fixture {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let (ca_cert_pem, ca_key_pem) = load_or_bootstrap_ca(
        dir.path().join("ca.crt").to_str().unwrap(),
        dir.path().join("ca.key").to_str().unwrap(),
        "Test CA",
    )
    .unwrap();
    let ca = RcgenCaClient::new(&ca_cert_pem, &ca_key_pem).unwrap();
    let tls = build_server_config(&ca, &ca_cert_pem, SERVER_IDENTITY)
        .await
        .unwrap();
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    let (listener, seen) = spawn_under_parent(ListenerArgs::tls(
        tcp,
        tokio_rustls::TlsAcceptor::from(tls),
        app(slow),
        drain_timeout,
    ))
    .await;
    Fixture {
        addr,
        ca_cert_pem,
        listener,
        seen,
    }
}

async fn spawn_under_parent(
    args: ListenerArgs,
) -> (ActorRef<ListenerMessage>, Arc<Mutex<Vec<Ending>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (parent, _) = Actor::spawn(None, Parent, seen.clone()).await.unwrap();
    let (listener, _) = Actor::spawn_linked(None, Listener, args, parent.get_cell())
        .await
        .unwrap();
    (listener, seen)
}

/// One TLS connection and one GET of `path`; the whole raw response, or the error connecting.
async fn get(f_addr: SocketAddr, ca_cert_pem: &str, path: &str) -> Result<String, String> {
    let mut roots = RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut ca_cert_pem.as_bytes()) {
        roots.add(cert.unwrap()).unwrap();
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tcp = TcpStream::connect(f_addr)
        .await
        .map_err(|e| e.to_string())?;
    let mut tls = TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from(SERVER_IDENTITY).unwrap(), tcp)
        .await
        .map_err(|e| e.to_string())?;
    tls.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: {SERVER_IDENTITY}\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .await
    .map_err(|e| e.to_string())?;
    let mut out = String::new();
    tls.read_to_string(&mut out)
        .await
        .map_err(|e| e.to_string())?;
    Ok(out)
}

async fn drain(listener: &ActorRef<ListenerMessage>) {
    ractor::call!(listener, ListenerMessage::Drain).unwrap();
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(100)).await;
}

#[tokio::test]
async fn the_listener_serves_until_it_is_drained_and_then_ends_without_failing() {
    let f = start(Duration::ZERO, Duration::from_secs(5)).await;
    let answer = get(f.addr, &f.ca_cert_pem, "/ping").await.unwrap();
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(answer.ends_with("pong"), "{answer}");

    let began = Instant::now();
    drain(&f.listener).await;
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "with nothing in flight the drain does not wait out its timeout ({:?})",
        began.elapsed()
    );
    settle().await;
    assert_eq!(f.listener.get_status(), ActorStatus::Stopped);
    assert_eq!(
        *f.seen.lock().unwrap(),
        vec![Ending::Terminated],
        "a drain is not a failure"
    );
    assert!(
        get(f.addr, &f.ca_cert_pem, "/ping").await.is_err(),
        "nothing listens once it is drained"
    );
}

#[tokio::test]
async fn a_drain_lets_the_requests_in_flight_finish() {
    let f = start(Duration::from_millis(600), Duration::from_secs(5)).await;
    let addr = f.addr;
    let ca = f.ca_cert_pem.clone();
    let in_flight = tokio::spawn(async move { get(addr, &ca, "/slow").await });
    settle().await;

    let began = Instant::now();
    drain(&f.listener).await;
    let drained = began.elapsed();
    assert!(
        drained >= Duration::from_millis(400),
        "the drain waited for the request in flight ({drained:?})"
    );
    assert!(
        drained < Duration::from_secs(3),
        "and no longer than it took, well inside its timeout ({drained:?})"
    );
    let answer = in_flight.await.unwrap().unwrap();
    assert!(
        answer.ends_with("done"),
        "the request in flight was answered: {answer}"
    );
}

#[tokio::test]
async fn a_drain_gives_up_on_requests_that_outlast_its_timeout() {
    let f = start(Duration::from_secs(30), Duration::from_millis(300)).await;
    let addr = f.addr;
    let ca = f.ca_cert_pem.clone();
    let _in_flight = tokio::spawn(async move { get(addr, &ca, "/slow").await });
    settle().await;

    let began = Instant::now();
    drain(&f.listener).await;
    assert!(
        began.elapsed() < Duration::from_secs(3),
        "the drain ended at its timeout ({:?})",
        began.elapsed()
    );
}

#[tokio::test]
async fn serving_that_ends_on_its_own_fails_the_listener() {
    let (listener, seen) = spawn_under_parent(ListenerArgs {
        serve: Box::new(|_stop, _active| Box::pin(async {})),
        drain_timeout: Duration::from_secs(1),
    })
    .await;
    settle().await;
    assert_eq!(listener.get_status(), ActorStatus::Stopped);
    let seen = seen.lock().unwrap().clone();
    assert!(
        matches!(seen.as_slice(), [Ending::Failed(why)] if why.contains("stopped serving")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn serving_that_panics_fails_the_listener() {
    let (listener, seen) = spawn_under_parent(ListenerArgs {
        serve: Box::new(|_stop, _active| Box::pin(async { panic!("the listener broke") })),
        drain_timeout: Duration::from_secs(1),
    })
    .await;
    settle().await;
    assert_eq!(listener.get_status(), ActorStatus::Stopped);
    let seen = seen.lock().unwrap().clone();
    assert!(
        matches!(seen.as_slice(), [Ending::Failed(why)] if why.contains("the listener broke")),
        "{seen:?}"
    );
}
