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

//! The one kind of message the daemon signs (the maintainer, 2026-10-27): the input to a TLS 1.3 CertificateVerify
//! (RFC 8446, section 4.4.3), which is 64 spaces, a context string, a zero byte and the transcript hash. A key that signs
//! only these can be used for handshakes and nothing else: nothing can be signed in the agent's name.

/// The context of a client's CertificateVerify.
pub const CLIENT_CONTEXT: &[u8] = b"TLS 1.3, client CertificateVerify";
/// The context of a server's CertificateVerify.
pub const SERVER_CONTEXT: &[u8] = b"TLS 1.3, server CertificateVerify";

/// The transcript hash lengths of TLS 1.3's suites: SHA-256 and SHA-384.
const HASH_LENGTHS: [usize; 2] = [32, 48];

/// Whether `message` is the input to a TLS 1.3 CertificateVerify, a client's or a server's.
pub fn is_tls13_certificate_verify(message: &[u8]) -> bool {
    let spaces = 64;
    if message.len() < spaces || !message[..spaces].iter().all(|b| *b == b' ') {
        return false;
    }
    let rest = &message[spaces..];
    [CLIENT_CONTEXT, SERVER_CONTEXT].iter().any(|context| {
        rest.len() > context.len()
            && rest.starts_with(context)
            && rest[context.len()] == 0
            && HASH_LENGTHS.contains(&(rest.len() - context.len() - 1))
    })
}

/// The input to a CertificateVerify over `transcript_hash`, for tests and for the benchmark.
pub fn certificate_verify_input(context: &[u8], transcript_hash: &[u8]) -> Vec<u8> {
    let mut message = vec![b' '; 64];
    message.extend_from_slice(context);
    message.push(0);
    message.extend_from_slice(transcript_hash);
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_or_server_certificate_verify_input_is_signed_with_either_hash() {
        for context in [CLIENT_CONTEXT, SERVER_CONTEXT] {
            for length in [32, 48] {
                assert!(is_tls13_certificate_verify(&certificate_verify_input(
                    context,
                    &vec![7; length]
                )));
            }
        }
    }

    #[test]
    fn anything_else_is_not() {
        let good = certificate_verify_input(CLIENT_CONTEXT, &[7; 32]);
        // A hash of another length; no zero byte; one space short; another context; TLS 1.2's input (no prefix).
        assert!(!is_tls13_certificate_verify(&certificate_verify_input(
            CLIENT_CONTEXT,
            &[7; 20]
        )));
        let mut no_zero = good.clone();
        no_zero[64 + CLIENT_CONTEXT.len()] = 1;
        assert!(!is_tls13_certificate_verify(&no_zero));
        assert!(!is_tls13_certificate_verify(&good[1..]));
        // The same length and context, with one of the 64 spaces changed.
        for at in [0, 31, 63] {
            let mut changed = good.clone();
            changed[at] = b'x';
            assert!(!is_tls13_certificate_verify(&changed), "space {at}");
        }
        assert!(!is_tls13_certificate_verify(&certificate_verify_input(
            b"TLS 1.3, other CertificateVerify",
            &[7; 32]
        )));
        assert!(!is_tls13_certificate_verify(&[7; 64]));
        assert!(!is_tls13_certificate_verify(b""));
    }
}
