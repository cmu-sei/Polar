/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

//! mTLS upgrade for cert-issuer's own listener.
//!
//! The listener stays a single bind address, serving the exact same
//! /issue and /validate routes it always has, completely unchanged
//! for any caller presenting no client certificate. A caller that
//! *does* present one -- verified against cert-issuer's own CA, the
//! same root that already signs every certificate this service
//! issues -- additionally gets its peer identity extracted and made
//! available to handler.rs, which is what /internal/issue gates on.
//!
//! Mirrors cassini-broker's own listener.rs mTLS setup and
//! extract_peer_identity function precisely -- same crypto provider
//! install, same WebPkiClientVerifier construction, same SAN-based
//! identity extraction. The one real difference:
//! `.allow_unauthenticated()`, since cassini's listener requires a
//! client cert on every connection and this one must not -- /issue
//! and /validate's existing bearer-token callers have never
//! presented one and must keep working exactly as before.
//!
//! cert-issuer mints its own TLS server certificate for itself at
//! startup, from the same in-process CA it already uses to sign
//! every other certificate it issues (RcgenCaClient::issue) -- no new
//! secret-mounted server cert/key files, no new bootstrap dependency.

use crate::ca::CaClient;
use cert_issuer_common::CertType;
use rustls::pki_types::CertificateDer;
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use std::sync::Arc;
use tokio_rustls::server::TlsStream;
use tokio::net::TcpStream;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, warn};

/// Mint cert-issuer's own TLS server certificate from its own
/// in-process CA and build the mTLS-capable ServerConfig. Called once
/// at startup.
///
/// `server_identity` becomes both the CSR's identity SAN and the
/// cert's own subject -- must be the DNS name callers will actually
/// connect to (cert-issuer's own Service DNS name in-cluster).
pub async fn build_server_config(
    ca: &dyn CaClient,
    ca_cert_pem: &str,
    server_identity: &str,
) -> Result<Arc<ServerConfig>, String> {
    let provider = rustls::crypto::ring::default_provider().install_default();
    if provider.is_err() {
        tracing::debug!("Crypto provider already configured");
    }

    // cert-issuer signs its own server cert with its own key, via the
    // exact same CaClient::issue path every other caller of /issue
    // goes through, and the exact same CSR-building logic every
    // client already uses (cert_issuer_common::keypair::generate_csr)
    // -- a genuine, self-signed-by-its-own-root CSR, not a shortcut
    // around the normal signing path or a second, duplicated
    // implementation of CSR generation.
    let csr_output = cert_issuer_common::keypair::generate_csr(
        server_identity,
        &cert_issuer_common::keypair::KeyAlgorithm::Ed25519,
        &[],
    )
    .map_err(|e| format!("failed to build cert-issuer's own CSR: {e}"))?;

    let issued = ca
        .issue(crate::ca::CaIssueRequest {
            csr_pem: csr_output.csr_pem,
            sans: vec![server_identity.to_string()],
            lifetime: std::time::Duration::from_secs(3600 * 24 * 365),
            cert_type: CertType::Server,
            subject_dn: Default::default(),
        })
        .await
        .map_err(|e| format!("failed to self-issue cert-issuer's own TLS cert: {e:?}"))?;

    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut issued.certificate_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("failed to parse own issued cert PEM: {e}"))?;
    let private_key = rustls_pemfile::private_key(&mut csr_output.private_key_pem.as_bytes())
        .map_err(|e| format!("failed to parse own generated key PEM: {e}"))?
        .ok_or_else(|| "no private key found in own generated key PEM".to_string())?;

    let mut root_store = RootCertStore::empty();
    let ca_certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut ca_cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("failed to parse own CA cert PEM: {e}"))?;
    for cert in ca_certs {
        root_store
            .add(cert)
            .map_err(|e| format!("failed to add own CA cert to root store: {e}"))?;
    }

    let verifier = WebPkiClientVerifier::builder(Arc::new(root_store))
        .allow_unauthenticated()
        .build()
        .map_err(|e| format!("failed to build client verifier: {e}"))?;

    let server_config = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, private_key)
        .map_err(|e| format!("failed to build TLS ServerConfig: {e}"))?;

    Ok(Arc::new(server_config))
}

/// Extracts the connecting client's identity from its verified peer
/// certificate's SAN, if one was presented at all. `None` for a
/// connection presenting no client cert -- the ordinary /issue and
/// /validate case -- or one whose SAN couldn't be parsed. Mirrors
/// cassini-broker's own listener.rs::extract_peer_identity precisely.
pub fn extract_peer_identity(stream: &TlsStream<TcpStream>) -> Option<String> {
    let (_, connection) = stream.get_ref();
    let peer_certs = connection.peer_certificates()?;
    let leaf_cert = peer_certs.first()?;

    use x509_parser::prelude::FromDer;
    let (_, parsed) = match x509_parser::certificate::X509Certificate::from_der(leaf_cert.as_ref()) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = ?e, "Failed to parse peer certificate for identity extraction");
            return None;
        }
    };

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


/// Serve `app` over TLS, forever. Every connection completes a TLS
/// handshake; a client certificate is optional (see build_server_config).
/// When one was presented and verified against our CA, its peer identity
/// is attached to that connection's requests as a `PeerIdentity`
/// extension, and by nothing else -- never from a header or the body,
/// which the caller controls. Connections without a verified client
/// certificate get no extension, and `/internal/issue` refuses them.
///
/// A plain-HTTP client fails the handshake and is dropped here.
pub async fn serve_tls(listener: TcpListener, acceptor: TlsAcceptor, app: axum::Router) {
    loop {
        let (tcp, remote) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                warn!(error = %e, "TCP accept failed");
                // Avoid a hot loop if the failure persists (fd exhaustion).
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };

        let acceptor = acceptor.clone();
        let app = app.clone();
        tokio::spawn(async move {
            let tls = match acceptor.accept(tcp).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(%remote, error = %e, "TLS handshake failed");
                    return;
                }
            };

            // Read before the stream is handed to hyper -- the peer
            // certificate is only reachable through the TlsStream.
            let app = match extract_peer_identity(&tls) {
                Some(identity) => app.layer(axum::Extension(crate::server::PeerIdentity(identity))),
                None => app,
            };

            let service = TowerToHyperService::new(app);
            if let Err(e) = Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(TokioIo::new(tls), service)
                .await
            {
                debug!(%remote, error = %e, "connection closed with error");
            }
        });
    }
}
