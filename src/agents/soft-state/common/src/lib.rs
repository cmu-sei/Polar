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

//! What an agent and the state agent say to each other (docs/architecture/soft-state.md, "Shape"): a closed, typed
//! request in an [`Envelope`] on [`REQUESTS_TOPIC`], and its reply, with the same correlation id, on the asker's own
//! reply topic, [`replies_topic`]. A request never carries Cypher: the state agent builds the query itself from a label
//! the asker may read and a property name it has checked.
//!
//! An asker uses [`ask`]: a fresh correlation id for each attempt (docs/architecture/cassini-request-reply-protocol.md,
//! "Retries"), the reply awaited for a bounded time on the router its own reply subscription delivers to.

pub use polar::request_reply::{Envelope, ReplyError, ReplyRouter};
use rkyv::{Archive, Deserialize, Serialize};

/// The state agent's workload name.
pub const OBSERVER_NAME: &str = "soft-state-observer";

/// Where every asker publishes its requests, and the state agent subscribes.
pub const REQUESTS_TOPIC: &str = "soft-state-requests";

/// The family of reply topics, one for each asker: each subscribes only to its own, so one agent's state is not
/// broadcast to the others (docs/architecture/topics.md, "Families").
pub const REPLIES_FAMILY: &str = "soft-state-replies.{workload}";

/// The reply topic of the workload `workload`.
pub fn replies_topic(workload: &str) -> String {
    REPLIES_FAMILY.replacen("{workload}", workload, 1)
}

/// What an agent asks of the state agent.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
pub enum StateRequest {
    /// The largest integer value of `property` over the nodes labelled `label`.
    Max { label: String, property: String },
}

/// What the state agent answers.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
pub enum StateReply {
    /// The value asked for; `None` when no node has it.
    Value(Option<i64>),
    /// A decision: the asker may not read that, or the request is malformed. Asking again changes nothing until the
    /// catalog or the policy does.
    Refused(String),
    /// The graph could not be read, or policy could not be consulted; the asker tries again.
    Unavailable(String),
}

/// What an asker got for one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The state agent answered.
    Replied(StateReply),
    /// No answer: the request could not be sent, or nothing came back in time.
    NoReply(String),
}

/// The bytes of a request, as it is published.
pub fn encode_request(envelope: &Envelope<StateRequest>) -> Vec<u8> {
    rkyv::to_bytes::<rkyv::rancor::Error>(envelope)
        .expect("a request of owned strings always serializes")
        .into_vec()
}

/// The bytes of a reply, as it is published.
pub fn encode_reply(envelope: &Envelope<StateReply>) -> Vec<u8> {
    rkyv::to_bytes::<rkyv::rancor::Error>(envelope)
        .expect("a reply of owned strings and integers always serializes")
        .into_vec()
}

/// The request in `payload`, or why it is not one. The bytes are validated before anything is read from them.
pub fn decode_request(payload: &[u8]) -> Result<Envelope<StateRequest>, String> {
    let mut aligned = rkyv::util::AlignedVec::<16>::new();
    aligned.extend_from_slice(payload);
    rkyv::from_bytes::<Envelope<StateRequest>, rkyv::rancor::Error>(&aligned)
        .map_err(|e| format!("not a state request: {e}"))
}

/// The reply in `payload`, or why it is not one.
pub fn decode_reply(payload: &[u8]) -> Result<Envelope<StateReply>, String> {
    let mut aligned = rkyv::util::AlignedVec::<16>::new();
    aligned.extend_from_slice(payload);
    rkyv::from_bytes::<Envelope<StateReply>, rkyv::rancor::Error>(&aligned)
        .map_err(|e| format!("not a state reply: {e}"))
}

/// One attempt at `request`: a fresh correlation id from `fresh_id`, registered on `router` before `publish` is given
/// the request's bytes, and the reply awaited for at most `timeout`. The asker's reply subscription delivers each reply
/// to `router` by its correlation id ([`deliver`]).
pub async fn ask(
    router: &ReplyRouter<StateReply>,
    request: StateRequest,
    fresh_id: impl FnOnce() -> String,
    timeout: std::time::Duration,
    publish: impl FnOnce(Vec<u8>) -> Result<(), String>,
) -> Answer {
    let correlation_id = fresh_id();
    let bytes = encode_request(&Envelope {
        correlation_id: correlation_id.clone(),
        payload: request,
    });
    match router
        .register_with_timeout(correlation_id, timeout, move || publish(bytes))
        .await
    {
        Ok(reply) => Answer::Replied(reply),
        Err(ReplyError::Timeout) => Answer::NoReply(format!(
            "the state agent did not answer within {}s",
            timeout.as_secs_f64()
        )),
        Err(e) => Answer::NoReply(e.to_string()),
    }
}

/// Hands a reply that arrived on the asker's reply topic to whoever waits for it. Bytes that are not a reply are
/// dropped, as a reply nobody waits for is.
pub fn deliver(router: &ReplyRouter<StateReply>, payload: &[u8]) -> Result<(), String> {
    let envelope = decode_reply(payload)?;
    router.deliver(&envelope.correlation_id, envelope.payload);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn each_asker_has_its_own_reply_topic() {
        assert_eq!(
            replies_topic("jira-observer"),
            "soft-state-replies.jira-observer"
        );
        assert_ne!(replies_topic("a"), replies_topic("b"));
    }

    fn request() -> impl Strategy<Value = Envelope<StateRequest>> {
        ("\\PC{0,24}", "\\PC{0,16}", "\\PC{0,16}").prop_map(|(correlation_id, label, property)| {
            Envelope {
                correlation_id,
                payload: StateRequest::Max { label, property },
            }
        })
    }

    fn reply() -> impl Strategy<Value = Envelope<StateReply>> {
        (
            "\\PC{0,24}",
            prop_oneof![
                any::<Option<i64>>().prop_map(StateReply::Value),
                "\\PC{0,32}".prop_map(StateReply::Refused),
                "\\PC{0,32}".prop_map(StateReply::Unavailable),
            ],
        )
            .prop_map(|(correlation_id, payload)| Envelope {
                correlation_id,
                payload,
            })
    }

    proptest! {
        /// A request reads back as it was sent.
        #[test]
        fn a_request_round_trips(sent in request()) {
            let got = decode_request(&encode_request(&sent)).unwrap();
            prop_assert_eq!(got.correlation_id, sent.correlation_id);
            prop_assert_eq!(got.payload, sent.payload);
        }

        /// A reply reads back as it was sent.
        #[test]
        fn a_reply_round_trips(sent in reply()) {
            let got = decode_reply(&encode_reply(&sent)).unwrap();
            prop_assert_eq!(got.correlation_id, sent.correlation_id);
            prop_assert_eq!(got.payload, sent.payload);
        }

        /// Bytes that are cut short are refused, never read as something else.
        #[test]
        fn a_truncated_request_is_refused(sent in request(), keep in 0usize..64) {
            let bytes = encode_request(&sent);
            prop_assume!(keep < bytes.len());
            prop_assert!(decode_request(&bytes[..keep]).is_err());
        }

        /// Arbitrary bytes are either refused or a well-formed request; decoding never panics.
        #[test]
        fn arbitrary_bytes_do_not_panic(bytes in prop::collection::vec(any::<u8>(), 0..96)) {
            let _ = decode_request(&bytes);
            let _ = decode_reply(&bytes);
        }
    }

    fn max() -> StateRequest {
        StateRequest::Max {
            label: "JiraIssue".into(),
            property: "updated_ms".into(),
        }
    }

    #[tokio::test]
    async fn an_answer_is_matched_by_its_correlation_id() {
        let router = Arc::new(ReplyRouter::<StateReply>::new());
        let delivering = router.clone();
        let answer = ask(
            &router,
            max(),
            || "c-1".to_string(),
            Duration::from_secs(5),
            move |bytes| {
                let sent = decode_request(&bytes).unwrap();
                assert_eq!(sent.payload, max());
                // Another asker's reply first, then this one's.
                for (id, value) in [("c-0", 1), (sent.correlation_id.as_str(), 42)] {
                    let reply = encode_reply(&Envelope {
                        correlation_id: id.to_string(),
                        payload: StateReply::Value(Some(value)),
                    });
                    deliver(&delivering, &reply).unwrap();
                }
                Ok(())
            },
        )
        .await;
        assert_eq!(answer, Answer::Replied(StateReply::Value(Some(42))));
    }

    #[tokio::test]
    async fn no_reply_in_time_or_no_publish_is_no_answer() {
        let router = ReplyRouter::<StateReply>::new();
        let answer = ask(
            &router,
            max(),
            || "c-2".to_string(),
            Duration::from_millis(20),
            |_| Ok(()),
        )
        .await;
        assert!(matches!(answer, Answer::NoReply(ref why) if why.contains("did not answer")));
        // A late reply finds nobody waiting.
        deliver(
            &router,
            &encode_reply(&Envelope {
                correlation_id: "c-2".into(),
                payload: StateReply::Value(None),
            }),
        )
        .unwrap();
        let answer = ask(
            &router,
            max(),
            || "c-3".to_string(),
            Duration::from_secs(5),
            |_| Err("the client is gone".to_string()),
        )
        .await;
        assert!(matches!(answer, Answer::NoReply(ref why) if why.contains("the client is gone")));
        assert!(deliver(&router, b"not a reply").is_err());
    }
}
