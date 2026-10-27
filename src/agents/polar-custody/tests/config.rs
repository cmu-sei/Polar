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

//! A workload's TLS configuration (docs/architecture/key-custody.md, step 2): with the daemon's key, a handshake the
//! daemon signs, and TLS 1.3 alone, the only handshakes it signs; with a key file, as before; and a daemon that does not
//! answer fails the signature rather than holding a handshake.

mod common;

use common::*;
use polar_custody::client::{ClientKey, CustodyClient, ServerKey, client_config, server_config};
use polar_custody::protocol::Slot;
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::version::{TLS12, TLS13};
use rustls::{RootCertStore, SupportedProtocolVersion};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

fn roots(ca: &TestCa) -> Arc<RootCertStore> {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_slice(ca.pem.as_bytes()).unwrap())
        .unwrap();
    Arc::new(roots)
}

fn verifier(ca: &TestCa) -> Arc<WebPkiServerVerifier> {
    WebPkiServerVerifier::builder(roots(ca)).build().unwrap()
}

fn chain(pem: &str) -> Vec<CertificateDer<'static>> {
    vec![CertificateDer::from_pem_slice(pem.as_bytes()).unwrap()]
}

/// A server under `ca` for one connection, demanding a client certificate from it and speaking only `versions`: the
/// name on the client's certificate, or why the handshake failed.
async fn server(
    ca: &TestCa,
    versions: &[&'static SupportedProtocolVersion],
) -> (SocketAddr, JoinHandle<Result<String, String>>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let certificate = params.signed_by(&key, &ca.issuer).unwrap();
    let config = rustls::ServerConfig::builder_with_protocol_versions(versions)
        .with_client_cert_verifier(
            rustls::server::WebPkiClientVerifier::builder(roots(ca))
                .build()
                .unwrap(),
        )
        .with_single_cert(
            vec![certificate.der().clone()],
            PrivateKeyDer::from_pem_slice(key.serialize_pem().as_bytes()).unwrap(),
        )
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let seen = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let tls = acceptor.accept(tcp).await.map_err(|e| e.to_string())?;
        let (_, connection) = tls.get_ref();
        let peer = connection
            .peer_certificates()
            .ok_or("no client certificate")?;
        Ok(first_dns_name(peer[0].as_ref()))
    });
    (address, seen)
}

async fn connect(config: rustls::ClientConfig, address: SocketAddr) -> Result<(), String> {
    let tcp = tokio::net::TcpStream::connect(address).await.unwrap();
    tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// polar-init's part: the daemon's CSR, certified and installed. The certificate, as the bundle carries it.
async fn installed(socket: std::path::PathBuf, ca: &TestCa) -> String {
    let client = CustodyClient::new(socket);
    let csr = tokio::task::spawn_blocking(move || {
        let csr = client.make_csr(Slot::Client, IDENTITY, &[]).unwrap();
        (client, csr)
    })
    .await
    .unwrap();
    let (client, csr) = csr;
    let certificate = certify(ca, &csr);
    let given = certificate.clone();
    tokio::task::spawn_blocking(move || client.install(Slot::Client, vec![given]).unwrap())
        .await
        .unwrap();
    certificate
}

#[tokio::test(flavor = "multi_thread")]
async fn with_the_daemon_s_key_the_handshake_is_signed_by_it() {
    let ca = test_ca();
    let daemon = daemon(&ca, both()).await;
    let certificate = installed(daemon.socket.clone(), &ca).await;
    let config = client_config(
        verifier(&ca),
        chain(&certificate),
        &ClientKey::Custody(daemon.socket.display().to_string()),
    )
    .unwrap();
    let (address, seen) = server(&ca, &[&TLS13]).await;
    connect(config, address).await.expect("the handshake");
    assert_eq!(seen.await.unwrap().unwrap(), IDENTITY);
}

#[tokio::test(flavor = "multi_thread")]
async fn with_the_daemon_s_key_only_tls13_is_offered() {
    let ca = test_ca();
    let daemon = daemon(&ca, both()).await;
    let certificate = installed(daemon.socket.clone(), &ca).await;
    let config = client_config(
        verifier(&ca),
        chain(&certificate),
        &ClientKey::Custody(daemon.socket.display().to_string()),
    )
    .unwrap();
    assert!(
        config
            .crypto_provider()
            .cipher_suites
            .iter()
            .any(|s| s.version() == &TLS12),
        "the provider has TLS 1.2 suites; the configuration must not offer them"
    );
    let (address, seen) = server(&ca, &[&TLS12]).await;
    assert!(connect(config, address).await.is_err());
    // Refused because it was never offered, not because the daemon then declined to sign a TLS 1.2 handshake.
    let refused = seen.await.unwrap().unwrap_err();
    assert!(
        refused.contains("Tls12NotOfferedOrEnabled"),
        "the client offered TLS 1.2: {refused}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn with_a_key_file_nothing_changes() {
    let ca = test_ca();
    let key = rcgen::KeyPair::generate().unwrap();
    let csr = rcgen::CertificateParams::new(vec![IDENTITY.to_string()])
        .unwrap()
        .serialize_request(&key)
        .unwrap()
        .pem()
        .unwrap();
    let certificate = certify(&ca, &csr);
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("key.pem");
    std::fs::write(&key_path, key.serialize_pem()).unwrap();
    let file = ClientKey::File(key_path.display().to_string());

    let (address, seen) = server(&ca, &[&TLS12]).await;
    let config = client_config(verifier(&ca), chain(&certificate), &file).unwrap();
    connect(config, address).await.expect("TLS 1.2, as before");
    assert_eq!(seen.await.unwrap().unwrap(), IDENTITY);

    let missing = ClientKey::File(dir.path().join("none.pem").display().to_string());
    let error = client_config(verifier(&ca), chain(&certificate), &missing).unwrap_err();
    assert!(error.contains("none.pem"), "names the file: {error}");
}

#[test]
fn a_daemon_that_does_not_answer_fails_the_signature_in_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("silent.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    // Accepts, reads nothing, answers nothing, and keeps the connection open.
    let held = std::thread::spawn(move || listener.accept().map(|(stream, _)| stream));
    let client = CustodyClient::new(&path).with_timeout(Duration::from_millis(200));
    let started = Instant::now();
    assert!(client.sign(Slot::Client, b"anything").is_err());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
    drop(held.join());
}

// The listeners (step 4): a server whose key is the daemon's server slot. A TLS 1.3 client verifying it against the CA
// is answered with the certificate installed there, and the handshake's signature is the daemon's.

/// polar-init's part for the server slot: the daemon's CSR for `name`, certified for server use, installed.
async fn installed_server(socket: std::path::PathBuf, ca: &TestCa, name: &str) -> String {
    let client = CustodyClient::new(socket);
    let names = vec![name.to_string()];
    let (client, csr) = tokio::task::spawn_blocking(move || {
        let csr = client.make_csr(Slot::Server, IDENTITY, &names).unwrap();
        (client, csr)
    })
    .await
    .unwrap();
    let mut request = rcgen::CertificateSigningRequestParams::from_pem(&csr).unwrap();
    request.params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let certificate = request.signed_by(&ca.issuer).unwrap().pem();
    let given = certificate.clone();
    tokio::task::spawn_blocking(move || client.install(Slot::Server, vec![given]).unwrap())
        .await
        .unwrap();
    certificate
}

fn client_verifier(ca: &TestCa) -> Arc<dyn rustls::server::danger::ClientCertVerifier> {
    rustls::server::WebPkiClientVerifier::builder(roots(ca))
        .build()
        .unwrap()
}

/// A client under `ca` speaking only `versions`, its own key in a file: the server's certificate it saw, or why not.
async fn client_of(
    ca: &TestCa,
    versions: &[&'static SupportedProtocolVersion],
    address: SocketAddr,
    name: &str,
) -> Result<CertificateDer<'static>, String> {
    let key = rcgen::KeyPair::generate().unwrap();
    let csr = rcgen::CertificateParams::new(vec![IDENTITY.to_string()])
        .unwrap()
        .serialize_request(&key)
        .unwrap()
        .pem()
        .unwrap();
    let config = rustls::ClientConfig::builder_with_protocol_versions(versions)
        .with_root_certificates(roots(ca))
        .with_client_auth_cert(
            chain(&certify(ca, &csr)),
            PrivateKeyDer::from_pem_slice(key.serialize_pem().as_bytes()).unwrap(),
        )
        .unwrap();
    let tcp = tokio::net::TcpStream::connect(address).await.unwrap();
    let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from(name.to_string()).unwrap(), tcp)
        .await
        .map_err(|e| e.to_string())?;
    Ok(tls.get_ref().1.peer_certificates().unwrap()[0]
        .clone()
        .into_owned())
}

/// A listener for one connection with `config`: why the handshake failed, if it did.
async fn serve_one(config: rustls::ServerConfig) -> (SocketAddr, JoinHandle<Result<(), String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let done = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        acceptor
            .accept(tcp)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
    (address, done)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_whose_key_is_the_daemon_s_is_verified_with_the_certificate_installed() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let ca = test_ca();
    let daemon = daemon_apart(&ca, both());
    let certificate = installed_server(daemon.socket.clone(), &ca, "localhost").await;
    let config = server_config(
        client_verifier(&ca),
        chain(&certificate),
        &ServerKey::Custody(daemon.socket.display().to_string()),
    )
    .unwrap();
    let (address, done) = serve_one(config).await;
    let seen = client_of(&ca, &[&TLS13], address, "localhost")
        .await
        .expect("the handshake, the server's signature the daemon's");
    done.await.unwrap().unwrap();
    assert_eq!(seen.as_ref(), chain(&certificate)[0].as_ref());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_whose_key_is_the_daemon_s_offers_tls13_alone() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let ca = test_ca();
    let daemon = daemon_apart(&ca, both());
    let certificate = installed_server(daemon.socket.clone(), &ca, "localhost").await;
    let config = server_config(
        client_verifier(&ca),
        chain(&certificate),
        &ServerKey::Custody(daemon.socket.display().to_string()),
    )
    .unwrap();
    let (address, done) = serve_one(config).await;
    assert!(
        client_of(&ca, &[&TLS12], address, "localhost")
            .await
            .is_err()
    );
    // Refused because the server does not speak it, not because the daemon then declined to sign.
    let refused = done.await.unwrap().unwrap_err();
    assert!(
        refused.contains("Tls12NotOfferedOrEnabled") || refused.contains("ProtocolVersion"),
        "the server offered TLS 1.2: {refused}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_with_a_key_file_is_as_before() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let ca = test_ca();
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let certificate = params.signed_by(&key, &ca.issuer).unwrap().pem();
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("key.pem");
    std::fs::write(&key_path, key.serialize_pem()).unwrap();
    let config = server_config(
        client_verifier(&ca),
        chain(&certificate),
        &ServerKey::File(key_path.display().to_string()),
    )
    .unwrap();
    let (address, done) = serve_one(config).await;
    client_of(&ca, &[&TLS12], address, "localhost")
        .await
        .expect("TLS 1.2, as before");
    done.await.unwrap().unwrap();
}

/// A custody in this process, its client slot holding a certificate from `ca`: what the daemon holds once polar-init
/// has installed it.
fn custody_installed(
    ca: &TestCa,
) -> (
    Arc<std::sync::RwLock<polar_custody::custody::Custody>>,
    String,
) {
    use polar_custody::custody::{Custody, Roles};
    use polar_custody::protocol::{Reply, Request};
    let mut custody = Custody::new(&ca.pem).unwrap();
    let installer = Roles {
        installer: true,
        workload: false,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let Reply::Csr(csr) = custody.handle(
        installer,
        Request::MakeCsr {
            slot: Slot::Client,
            identity: IDENTITY.to_string(),
            names: vec![],
        },
        now,
    ) else {
        panic!("a CSR");
    };
    let certificate = certify(ca, &csr);
    assert_eq!(
        custody.handle(
            installer,
            Request::Install {
                slot: Slot::Client,
                chain_pem: vec![certificate.clone()],
            },
            now,
        ),
        Reply::Installed
    );
    (Arc::new(std::sync::RwLock::new(custody)), certificate)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_s_own_handshake_is_signed_in_process_with_tls13_alone() {
    let ca = test_ca();
    let (custody, _) = custody_installed(&ca);
    let config = polar_custody::own::own_client_config(custody, &ca.pem).unwrap();
    let (address, seen) = server(&ca, &[&TLS13]).await;
    connect(config.clone(), address)
        .await
        .expect("the handshake");
    assert_eq!(seen.await.unwrap().unwrap(), IDENTITY);
    let (address, seen) = server(&ca, &[&TLS12]).await;
    assert!(connect(config, address).await.is_err());
    // Refused because it was never offered, not because the custody then declined to sign a TLS 1.2 handshake.
    let refused = seen.await.unwrap().unwrap_err();
    assert!(
        refused.contains("Tls12NotOfferedOrEnabled"),
        "the daemon's own connection offered TLS 1.2: {refused}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn once_revoked_the_daemon_s_own_handshake_fails() {
    let ca = test_ca();
    let (custody, certificate) = custody_installed(&ca);
    let config = polar_custody::own::own_client_config(custody.clone(), &ca.pem).unwrap();
    let serial = cert_issuer_common::serial::serial_from_pem(&certificate).unwrap();
    custody.write().unwrap().revoke(&[serial]);
    let (address, seen) = server(&ca, &[&TLS13]).await;
    assert!(connect(config, address).await.is_err());
    assert!(seen.await.unwrap().is_err(), "no certificate seen signed");
}

#[test]
fn the_daemon_s_own_configuration_needs_a_certificate_installed() {
    let ca = test_ca();
    let custody = Arc::new(std::sync::RwLock::new(
        polar_custody::custody::Custody::new(&ca.pem).unwrap(),
    ));
    assert!(polar_custody::own::own_client_config(custody, &ca.pem).is_err());
}
