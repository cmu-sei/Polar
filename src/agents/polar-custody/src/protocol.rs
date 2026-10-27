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

//! What a caller asks of the daemon and what it is told, and how each is framed on the socket: a four-byte big-endian
//! length, then the rkyv bytes. Every request carries an id, which its reply repeats, so a caller may have many in
//! flight on one connection.

use rkyv::{Archive, Deserialize, Serialize};
use std::io::{Read, Write};

/// The largest frame either side sends or takes: a certificate chain fits many times over.
pub const MAX_FRAME: usize = 64 * 1024;

/// Which key: a pod may hold a client certificate and a server certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Archive, Serialize, Deserialize)]
pub enum Slot {
    Client,
    Server,
}

#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
pub enum Request {
    /// A new key for `slot` (any key the slot held goes), and a CSR for it: `polar-init`'s.
    MakeCsr {
        slot: Slot,
        identity: String,
        names: Vec<String>,
    },
    /// The certificate issued for `slot`'s key, then whatever links it to the CA: `polar-init`'s.
    Install { slot: Slot, chain_pem: Vec<String> },
    /// The certificate chain installed for `slot`.
    Certificate { slot: Slot },
    /// A signature with `slot`'s key over `message`, a TLS 1.3 CertificateVerify input: the workload's.
    Sign { slot: Slot, message: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
pub enum Reply {
    Csr(String),
    Installed,
    Certificate(Vec<String>),
    /// A DER-encoded ECDSA P-256 SHA-256 signature.
    Signature(Vec<u8>),
    Refused(Refusal),
}

#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize, thiserror::Error)]
pub enum Refusal {
    #[error("this caller may not ask that")]
    NotPermitted,
    #[error("the slot holds no key")]
    NoKey,
    #[error("the slot's key has no certificate")]
    NoCertificate,
    #[error("only a TLS 1.3 CertificateVerify input is signed")]
    NotTls13Handshake,
    #[error("the certificate is refused: {0}")]
    BadCertificate(String),
    #[error("the request is refused: {0}")]
    BadRequest(String),
    /// The slot holds an installed certificate: no new key, no other certificate, for the life of the daemon
    /// (docs/architecture/key-custody.md, step 4, item 1).
    #[error("the slot is sealed: a certificate is installed in it")]
    Sealed,
}

#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub id: u64,
    pub body: T,
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("the connection: {0}")]
    Io(#[from] std::io::Error),
    #[error("a frame of {0} bytes, more than {MAX_FRAME}")]
    TooLarge(usize),
    #[error("a frame that does not read: {0}")]
    Malformed(String),
}

/// The bytes of one frame for `envelope`, its length first.
pub fn frame<T>(envelope: &Envelope<T>) -> Result<Vec<u8>, FrameError>
where
    T: for<'a> Serialize<
        rkyv::api::high::HighSerializer<
            rkyv::util::AlignedVec,
            rkyv::ser::allocator::ArenaHandle<'a>,
            rkyv::rancor::Error,
        >,
    >,
{
    let body = rkyv::to_bytes::<rkyv::rancor::Error>(envelope)
        .map_err(|e| FrameError::Malformed(e.to_string()))?;
    if body.len() > MAX_FRAME {
        return Err(FrameError::TooLarge(body.len()));
    }
    let mut bytes = (body.len() as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(&body);
    Ok(bytes)
}

/// The length a frame's first four bytes say, if it is one this side takes.
pub fn frame_length(header: [u8; 4]) -> Result<usize, FrameError> {
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_FRAME {
        return Err(FrameError::TooLarge(length));
    }
    Ok(length)
}

pub fn decode_request(body: &[u8]) -> Result<Envelope<Request>, FrameError> {
    rkyv::from_bytes::<Envelope<Request>, rkyv::rancor::Error>(body)
        .map_err(|e| FrameError::Malformed(e.to_string()))
}

pub fn decode_reply(body: &[u8]) -> Result<Envelope<Reply>, FrameError> {
    rkyv::from_bytes::<Envelope<Reply>, rkyv::rancor::Error>(body)
        .map_err(|e| FrameError::Malformed(e.to_string()))
}

/// Writes one frame, blocking.
pub fn write_frame<W: Write, T>(writer: &mut W, envelope: &Envelope<T>) -> Result<(), FrameError>
where
    T: for<'a> Serialize<
        rkyv::api::high::HighSerializer<
            rkyv::util::AlignedVec,
            rkyv::ser::allocator::ArenaHandle<'a>,
            rkyv::rancor::Error,
        >,
    >,
{
    writer.write_all(&frame(envelope)?)?;
    Ok(())
}

/// Reads one frame's body, blocking.
pub fn read_frame<R: Read>(reader: &mut R) -> Result<Vec<u8>, FrameError> {
    let mut header = [0u8; 4];
    reader.read_exact(&mut header)?;
    let mut body = vec![0u8; frame_length(header)?];
    reader.read_exact(&mut body)?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_and_a_reply_survive_their_frames() {
        let request = Envelope {
            id: 7,
            body: Request::Sign {
                slot: Slot::Server,
                message: vec![1, 2, 3],
            },
        };
        let bytes = frame(&request).unwrap();
        let mut reader = bytes.as_slice();
        assert_eq!(
            decode_request(&read_frame(&mut reader).unwrap()).unwrap(),
            request
        );
        let reply = Envelope {
            id: 7,
            body: Reply::Refused(Refusal::NotTls13Handshake),
        };
        let bytes = frame(&reply).unwrap();
        let mut reader = bytes.as_slice();
        assert_eq!(
            decode_reply(&read_frame(&mut reader).unwrap()).unwrap(),
            reply
        );
    }

    #[test]
    fn a_frame_too_large_is_refused_before_it_is_read() {
        assert!(matches!(
            frame_length(((MAX_FRAME + 1) as u32).to_be_bytes()),
            Err(FrameError::TooLarge(_))
        ));
        assert!(
            decode_request(&[1, 2, 3]).is_err(),
            "bytes that are not a request"
        );
    }
}
