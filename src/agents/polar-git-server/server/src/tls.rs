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

//! The mTLS listener. A client certificate is required on every connection, verified against the
//! Polar CA; the peer identity is read from the verified certificate and attached to the
//! connection's requests, and from nothing else (never a header or a body, which the caller
//! controls). Follows Cassini's `listener.rs` and cert-issuer's `tls.rs`, except that there is no
//! `allow_unauthenticated`: an anonymous caller does not get through the handshake.

use crate::http::PeerIdentity;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use polar_custody::client::ServerKey;
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

/// Builds the server configuration: the server's certificate chain (PEM), its key (a PEM file, or the custody
/// daemon's server slot, which offers TLS 1.3 alone: docs/architecture/key-custody.md, step 4), and the CA that client
/// certificates must chain to.
pub fn server_config(cert: &Path, key: &ServerKey, ca: &Path) -> Result<Arc<ServerConfig>, String> {
    // Another component may have installed it already.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let read = |p: &Path| std::fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()));
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut read(cert)?.as_slice())
        .collect::<Result<_, _>>()
        .map_err(|e| format!("bad server certificate: {e}"))?;

    let mut roots = RootCertStore::empty();
    for ca in rustls_pemfile::certs(&mut read(ca)?.as_slice()) {
        roots
            .add(ca.map_err(|e| format!("bad CA certificate: {e}"))?)
            .map_err(|e| format!("cannot trust the CA certificate: {e}"))?;
    }
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| format!("cannot build the client verifier: {e}"))?;

    let mut config = polar_custody::client::server_config(verifier, certs, key)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    // Smart HTTP over HTTP/1.1, whichever holds the key (docs/architecture/key-custody.md, step 4).
    #[test]
    fn the_server_speaks_http_1_1_with_a_key_file_or_the_daemon_s() {
        let dir = tempfile::tempdir().unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let (cert_path, key_path, ca_path) = (
            dir.path().join("cert.pem"),
            dir.path().join("key.pem"),
            dir.path().join("ca.pem"),
        );
        std::fs::write(&cert_path, cert.pem()).unwrap();
        std::fs::write(&key_path, key.serialize_pem()).unwrap();
        std::fs::write(&ca_path, cert.pem()).unwrap();
        for key in [
            ServerKey::File(key_path.display().to_string()),
            // Built without asking the daemon: it is asked in each handshake.
            ServerKey::Custody(dir.path().join("custody.sock").display().to_string()),
        ] {
            let config = server_config(&cert_path, &key, &ca_path).unwrap();
            assert_eq!(config.alpn_protocols, vec![b"http/1.1".to_vec()], "{key:?}");
        }
    }
}
