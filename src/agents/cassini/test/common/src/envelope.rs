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

//! The message a producer sends: who sent it, its place in that producer's sequence, when it was sent, and the
//! payload. The header is a fixed 20 bytes, big-endian, in front of the payload, so the sink reads what it measures
//! without decoding, or copying, the payload: a sink that spent its time decoding would measure itself, not the
//! broker.

/// The header's length: the producer (4 bytes), the sequence number (8) and the send time (8).
pub const HEADER_LEN: usize = 20;

/// What the sink reads of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    /// The producer's index in the run.
    pub producer: u32,
    /// Its place in the producer's sequence, from 0.
    pub seqno: u64,
    /// When it was sent, in nanoseconds since the Unix epoch, by the producer's clock.
    pub sent_at_ns: u64,
    /// How long the payload after the header is.
    pub payload_len: usize,
}

/// The message `producer` sends as its `seqno`th, at `sent_at_ns`, carrying `payload`.
pub fn encode(producer: u32, seqno: u64, sent_at_ns: u64, payload: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(HEADER_LEN + payload.len());
    message.extend_from_slice(&producer.to_be_bytes());
    message.extend_from_slice(&seqno.to_be_bytes());
    message.extend_from_slice(&sent_at_ns.to_be_bytes());
    message.extend_from_slice(payload);
    message
}

/// The stamp of `message`, or why it is not one of the harness's messages.
pub fn read(message: &[u8]) -> Result<Stamp, String> {
    if message.len() < HEADER_LEN {
        return Err(format!(
            "a message of {} bytes is shorter than the harness's {HEADER_LEN}-byte header",
            message.len()
        ));
    }
    let word = |at: usize| u64::from_be_bytes(message[at..at + 8].try_into().expect("8 bytes"));
    Ok(Stamp {
        producer: u32::from_be_bytes(message[0..4].try_into().expect("4 bytes")),
        seqno: word(4),
        sent_at_ns: word(12),
        payload_len: message.len() - HEADER_LEN,
    })
}

/// Now, in nanoseconds since the Unix epoch: a message's send time, and its receipt.
pub fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// A payload of `size` bytes that does not compress, the same every run: made once per run, not per message.
pub fn payload(size: usize) -> Vec<u8> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..size)
        .map(|_| {
            // xorshift64: enough to defeat compression, and nothing to do with security.
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn the_payload_is_the_size_asked_for_and_the_same_each_time() {
        assert_eq!(payload(0), Vec::<u8>::new());
        assert_eq!(payload(4096).len(), 4096);
        assert_eq!(payload(64), payload(64));
    }

    proptest! {
        /// What is encoded is read back, whatever it carries.
        #[test]
        fn a_message_reads_back_as_it_was_written(
            producer: u32,
            seqno: u64,
            sent_at_ns: u64,
            body in prop::collection::vec(any::<u8>(), 0..256),
        ) {
            let stamp = read(&encode(producer, seqno, sent_at_ns, &body)).unwrap();
            prop_assert_eq!(stamp, Stamp { producer, seqno, sent_at_ns, payload_len: body.len() });
        }

        /// Anything shorter than the header is refused, and says how long it was.
        #[test]
        fn a_message_shorter_than_the_header_is_refused(short in prop::collection::vec(any::<u8>(), 0..HEADER_LEN)) {
            let said = read(&short).unwrap_err();
            prop_assert!(said.contains(&format!("{} bytes", short.len())), "{}", said);
        }
    }
}
