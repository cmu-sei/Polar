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

//! The workload's side of the daemon (docs/architecture/key-custody.md, "Step 1 in detail"): a blocking client, since
//! rustls calls its signer synchronously in the handshake, with a few connections kept open so that no signature pays
//! for a connection; and a rustls signing key over it, so a TLS configuration takes the daemon's key as it would a
//! key file. Only TLS 1.3 handshakes are signed, so the configurations that use it offer TLS 1.3 alone.

use crate::protocol::{
    Envelope, FrameError, Refusal, Reply, Request, Slot, decode_reply, read_frame, write_frame,
};
use rustls::pki_types::CertificateDer;
use rustls::sign::{CertifiedKey, Signer, SigningKey};
use rustls::{SignatureAlgorithm, SignatureScheme};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The most idle connections kept.
const IDLE: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("the reply was for another request")]
    Mismatched,
    #[error("refused: {0}")]
    Refused(Refusal),
    #[error("an answer of another kind: {0:?}")]
    Unexpected(Box<Reply>),
}

#[derive(Debug)]
pub struct CustodyClient {
    path: PathBuf,
    idle: Mutex<Vec<UnixStream>>,
    next: AtomicU64,
}

impl CustodyClient {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            idle: Mutex::new(Vec::new()),
            next: AtomicU64::new(1),
        }
    }

    /// One request and its reply, on an idle connection or a new one. A connection that fails is dropped.
    pub fn request(&self, body: Request) -> Result<Reply, ClientError> {
        let idle = self.idle.lock().ok().and_then(|mut idle| idle.pop());
        let mut stream = match idle {
            Some(stream) => stream,
            None => UnixStream::connect(&self.path).map_err(FrameError::Io)?,
        };
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        write_frame(&mut stream, &Envelope { id, body })?;
        let reply = decode_reply(&read_frame(&mut stream)?)?;
        if reply.id != id {
            return Err(ClientError::Mismatched);
        }
        if let Ok(mut idle) = self.idle.lock()
            && idle.len() < IDLE
        {
            idle.push(stream);
        }
        Ok(reply.body)
    }

    pub fn make_csr(
        &self,
        slot: Slot,
        identity: &str,
        names: &[String],
    ) -> Result<String, ClientError> {
        match self.request(Request::MakeCsr {
            slot,
            identity: identity.to_string(),
            names: names.to_vec(),
        })? {
            Reply::Csr(csr) => Ok(csr),
            other => Err(refused_or_unexpected(other)),
        }
    }

    pub fn install(&self, slot: Slot, chain_pem: Vec<String>) -> Result<(), ClientError> {
        match self.request(Request::Install { slot, chain_pem })? {
            Reply::Installed => Ok(()),
            other => Err(refused_or_unexpected(other)),
        }
    }

    pub fn certificate(&self, slot: Slot) -> Result<Vec<String>, ClientError> {
        match self.request(Request::Certificate { slot })? {
            Reply::Certificate(chain) => Ok(chain),
            other => Err(refused_or_unexpected(other)),
        }
    }

    pub fn sign(&self, slot: Slot, message: &[u8]) -> Result<Vec<u8>, ClientError> {
        match self.request(Request::Sign {
            slot,
            message: message.to_vec(),
        })? {
            Reply::Signature(signature) => Ok(signature),
            other => Err(refused_or_unexpected(other)),
        }
    }
}

fn refused_or_unexpected(reply: Reply) -> ClientError {
    match reply {
        Reply::Refused(refusal) => ClientError::Refused(refusal),
        other => ClientError::Unexpected(Box::new(other)),
    }
}

/// A rustls signing key whose key is the daemon's: it signs ECDSA P-256 SHA-256, and only TLS 1.3 handshakes.
#[derive(Debug, Clone)]
pub struct CustodySigningKey {
    client: Arc<CustodyClient>,
    slot: Slot,
}

impl CustodySigningKey {
    pub fn new(client: Arc<CustodyClient>, slot: Slot) -> Self {
        Self { client, slot }
    }
}

impl SigningKey for CustodySigningKey {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
        offered
            .contains(&SignatureScheme::ECDSA_NISTP256_SHA256)
            .then(|| Box::new(self.clone()) as Box<dyn Signer>)
    }

    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::ECDSA
    }
}

impl Signer for CustodySigningKey {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rustls::Error> {
        self.client
            .sign(self.slot, message)
            .map_err(|e| rustls::Error::General(format!("the custody daemon did not sign: {e}")))
    }

    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::ECDSA_NISTP256_SHA256
    }
}

/// The slot's certificate chain and a signing key over the daemon, for a rustls configuration.
pub fn certified_key(client: Arc<CustodyClient>, slot: Slot) -> Result<CertifiedKey, ClientError> {
    use rustls::pki_types::pem::PemObject as _;
    let chain = client
        .certificate(slot)?
        .iter()
        .map(|pem| {
            CertificateDer::from_pem_slice(pem.as_bytes())
                .map_err(|e| ClientError::Frame(FrameError::Malformed(e.to_string())))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CertifiedKey::new(
        chain,
        Arc::new(CustodySigningKey::new(client, slot)),
    ))
}

/// Hands rustls the daemon's certificate and key whenever a server asks for a client certificate.
#[derive(Debug)]
pub struct CustodyClientCert(pub Arc<CertifiedKey>);

impl rustls::client::ResolvesClientCert for CustodyClientCert {
    fn resolve(&self, _: &[&[u8]], _: &[SignatureScheme]) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }

    fn has_certs(&self) -> bool {
        true
    }
}
