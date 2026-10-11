//! The mTLS listener. A client certificate is required on every connection, verified against the
//! Polar CA; the peer identity is read from the verified certificate and attached to the
//! connection's requests, and from nothing else (never a header or a body, which the caller
//! controls). Follows Cassini's `listener.rs` and cert-issuer's `tls.rs`, except that there is no
//! `allow_unauthenticated`: an anonymous caller does not get through the handshake.

use crate::http::PeerIdentity;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use rustls::pki_types::CertificateDer;
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;
use tracing::{debug, warn};

/// Builds the server configuration from PEM files: the server's certificate chain and key, and
/// the CA that client certificates must chain to.
pub fn server_config(cert: &Path, key: &Path, ca: &Path) -> Result<Arc<ServerConfig>, String> {
    // Another component may have installed it already.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let read = |p: &Path| std::fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()));
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut read(cert)?.as_slice())
        .collect::<Result<_, _>>()
        .map_err(|e| format!("bad server certificate: {e}"))?;
    let key = rustls_pemfile::private_key(&mut read(key)?.as_slice())
        .map_err(|e| format!("bad server key: {e}"))?
        .ok_or("no private key in the server key file")?;

    let mut roots = RootCertStore::empty();
    for ca in rustls_pemfile::certs(&mut read(ca)?.as_slice()) {
        roots
            .add(ca.map_err(|e| format!("bad CA certificate: {e}"))?)
            .map_err(|e| format!("cannot trust the CA certificate: {e}"))?;
    }
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| format!("cannot build the client verifier: {e}"))?;

    let mut config = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)
        .map_err(|e| format!("bad server certificate or key: {e}"))?;
    // Smart HTTP over HTTP/1.1 is all git needs.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// The first DNS name in the verified client certificate's SANs.
pub fn extract_peer_identity(stream: &TlsStream<TcpStream>) -> Option<String> {
    let (_, connection) = stream.get_ref();
    let leaf = connection.peer_certificates()?.first()?;
    use x509_parser::prelude::FromDer;
    let (_, parsed) = x509_parser::certificate::X509Certificate::from_der(leaf.as_ref()).ok()?;
    for ext in parsed.extensions() {
        if let x509_parser::extensions::ParsedExtension::SubjectAlternativeName(san) =
            ext.parsed_extension()
        {
            for name in &san.general_names {
                if let x509_parser::extensions::GeneralName::DNSName(dns) = name {
                    return Some(dns.to_string());
                }
            }
        }
    }
    None
}

/// Serves `app` over TLS until `stop` is notified. Each connection that completes the handshake
/// with an identity gets it as a [`PeerIdentity`] extension; one whose certificate carries no
/// identity is dropped, since nothing could authorize it.
pub async fn serve_tls(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    app: axum::Router,
    stop: Arc<Notify>,
    active: Arc<AtomicUsize>,
) {
    loop {
        let (tcp, remote) = tokio::select! {
            _ = stop.notified() => return,
            accepted = listener.accept() => match accepted {
                Ok(conn) => conn,
                Err(e) => {
                    warn!(error = %e, "TCP accept failed");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            },
        };
        let acceptor = acceptor.clone();
        let app = app.clone();
        let guard = Connection::open(&active);
        tokio::spawn(async move {
            let _guard = guard;
            let tls = match acceptor.accept(tcp).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(%remote, error = %e, "TLS handshake failed");
                    return;
                }
            };
            let Some(identity) = extract_peer_identity(&tls) else {
                warn!(%remote, "client certificate carries no identity; connection dropped");
                return;
            };
            let app = app.layer(axum::Extension(PeerIdentity(identity)));
            let service = TowerToHyperService::new(app);
            if let Err(e) = Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(tls), service)
                .await
            {
                debug!(%remote, error = %e, "connection closed with error");
            }
        });
    }
}

/// Counts a connection as in flight until it is dropped, so shutdown can wait for them.
struct Connection(Arc<AtomicUsize>);

impl Connection {
    fn open(active: &Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, Ordering::SeqCst);
        Self(active.clone())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
