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

//! Self-state: what an agent keeps in the graph about its own work and reads back through the state agent
//! (docs/architecture/soft-state.md, "Self-state and introspection"). The [`StateActor`] runs beside the
//! ConfigurationActor, under the InfrastructureSupervisor, as the CryptoActor does, and is not part of it: what the
//! ConfigurationActor holds is signed, verified and versioned, and soft state is an unsigned hint that may be lost, so
//! the two are kept apart, and [`SoftValue`] is a type of its own that cannot stand where a
//! [`RuntimeValue`](crate::RuntimeValue) is expected.
//!
//! The actor owns the request and the reply: a fresh correlation id for each attempt, the wait, the attempts, and the
//! check that a reply comes from the state agent's identity as the broker names it. The builder starts it for an agent
//! that declares a graph read (`PolarAgent::GRAPH`), which is what gives the agent the `soft-state` capability and its
//! topics (`topics.rs`, `Rule::SoftState`); the agent asks through its [`StateHandle`].
//!
//! For now an agent can ask `get` (the largest value of a property over a label); `changes` and `put` come with the
//! state agent's writer half.

use crate::Subscription;
use async_trait::async_trait;
use cassini_client::{OfflineBehavior, PublishRequest};
use cassini_types::WireTraceCtx;
use polar::cassini::{CassiniClient, TcpClient};
use polar_env::{Defaulted, Described, Env, Section};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use soft_state_common::{Answer, ReplyRouter, StateReply, StateRequest};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

/// How long one attempt waits for the state agent's answer.
pub static REPLY_TIMEOUT_SECS: Defaulted<u64> = Defaulted::seconds(
    "POLAR_STATE_REPLY_TIMEOUT_SECS",
    "how long the agent's state actor waits for the state agent to answer one question, in seconds; an attempt not \
     answered by then is tried again with a fresh correlation id (docs/architecture/soft-state.md)",
    "8",
);

/// How many attempts one question gets before the asker is told nothing came back.
pub static ATTEMPTS: Defaulted<u64> = Defaulted::number(
    "POLAR_STATE_ATTEMPTS",
    "how many times the agent's state actor asks the state agent one question, each with a fresh correlation id, \
     before it says no answer came; an answer (a value, a refusal, an unavailable graph) is never asked again",
    "3",
);

static VARIABLES: &[&dyn Described] = &[&REPLY_TIMEOUT_SECS, &ATTEMPTS];

/// The section the `soft-state` capability brings.
pub static SECTION: Section = Section {
    name: "soft-state",
    vars: VARIABLES,
    relations,
};

fn relations(env: &dyn Env) -> Vec<String> {
    let mut problems = Vec::new();
    if ATTEMPTS.read(env).is_ok_and(|n| n == 0) {
        problems.push(format!("{} must be at least 1", ATTEMPTS.name()));
    }
    if REPLY_TIMEOUT_SECS.read(env).is_ok_and(|n| n == 0) {
        problems.push(format!("{} must be at least 1", REPLY_TIMEOUT_SECS.name()));
    }
    problems
}

/// The state actor's settings, read from the agent's snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateSettings {
    pub reply_timeout: Duration,
    pub attempts: u64,
}

impl StateSettings {
    pub fn from_env(env: &dyn Env) -> Result<Self, String> {
        Ok(Self {
            reply_timeout: Duration::from_secs(
                REPLY_TIMEOUT_SECS
                    .read(env)
                    .map_err(|e| e.to_string())?
                    .max(1),
            ),
            attempts: ATTEMPTS.read(env).map_err(|e| e.to_string())?.max(1),
        })
    }
}

/// What an agent asks of its own soft state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateQuery {
    /// The largest integer value of `property` over the nodes labelled `label`.
    Max { label: String, property: String },
}

impl StateQuery {
    fn wire(&self) -> StateRequest {
        match self {
            StateQuery::Max { label, property } => StateRequest::Max {
                label: label.clone(),
                property: property.clone(),
            },
        }
    }
}

/// A value read back from soft state: a hint, never signed configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoftValue {
    largest: Option<i64>,
}

impl SoftValue {
    /// The largest value asked for; `None` when no node has it.
    pub fn largest(&self) -> Option<i64> {
        self.largest
    }

    /// A value, for a test's stand-in for the state agent.
    pub fn of(largest: Option<i64>) -> Self {
        Self { largest }
    }
}

/// Why no value came back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    /// The state agent decided: the agent may not read that, or the question is malformed.
    Refused(String),
    /// The state agent could not answer (its graph, or its policy); ask again later.
    Unavailable(String),
    /// Nothing came back, in every attempt.
    NoReply(String),
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateError::Refused(why) => write!(f, "the state agent refused: {why}"),
            StateError::Unavailable(why) => write!(f, "the state agent could not answer: {why}"),
            StateError::NoReply(why) => write!(f, "no answer from the state agent: {why}"),
        }
    }
}

impl std::error::Error for StateError {}

/// How an agent reads its soft state: [`StateHandle`], or a test's stand-in.
#[async_trait]
pub trait StateClient: Send + Sync {
    async fn get(&self, query: StateQuery) -> Result<SoftValue, StateError>;
}

/// The messages of the [`StateActor`].
pub enum StateMessage {
    Get {
        query: StateQuery,
        reply: RpcReplyPort<Result<SoftValue, StateError>>,
    },
}

/// The agent's way to its state actor.
#[derive(Clone)]
pub struct StateHandle {
    actor: ActorRef<StateMessage>,
}

impl StateHandle {
    pub fn new(actor: ActorRef<StateMessage>) -> Self {
        Self { actor }
    }

    pub fn as_actor_ref(&self) -> ActorRef<StateMessage> {
        self.actor.clone()
    }
}

#[async_trait]
impl StateClient for StateHandle {
    async fn get(&self, query: StateQuery) -> Result<SoftValue, StateError> {
        match ractor::call!(self.actor, |reply| StateMessage::Get { query, reply }) {
            Ok(result) => result,
            Err(e) => Err(StateError::NoReply(format!(
                "the state actor is not running: {e}"
            ))),
        }
    }
}

/// What one attempt's answer means: a value, a decision, or nothing.
pub fn outcome(answer: Answer) -> Result<SoftValue, StateError> {
    match answer {
        Answer::Replied(StateReply::Value(largest)) => Ok(SoftValue { largest }),
        Answer::Replied(StateReply::Refused(why)) => Err(StateError::Refused(why)),
        Answer::Replied(StateReply::Unavailable(why)) => Err(StateError::Unavailable(why)),
        Answer::NoReply(why) => Err(StateError::NoReply(why)),
    }
}

/// The state agent's identity in the agent's own namespace and domain: `soft-state-observer.<namespace>.serviceaccount.
/// <domain>` for an agent whose identity is `<workload>.<namespace>.serviceaccount.<domain>`. None for an identity that
/// names no workload.
pub fn state_agent_identity(own_identity: &str) -> Option<String> {
    let workload = cert_issuer_common::identity::workload_of(own_identity)?;
    Some(format!(
        "{}{}",
        soft_state_common::OBSERVER_NAME,
        &own_identity[workload.len()..]
    ))
}

/// Whether a reply was published by the state agent, `expected`, as the broker names its publisher. Anyone else's, or
/// one the broker vouched for no one, is not an answer.
pub fn from_state_agent(expected: &str, published_by: Option<&str>) -> bool {
    published_by == Some(expected)
}

/// The subscription to the agent's own reply topic, handing each reply from the state agent (`expected`, its
/// identity) to `router`.
pub fn replies_subscription(
    service_name: &str,
    expected: String,
    router: Arc<ReplyRouter<StateReply>>,
) -> Subscription {
    let topic = soft_state_common::replies_topic(service_name);
    let named = topic.clone();
    Subscription::new(topic, move |incoming| {
        if !from_state_agent(&expected, incoming.published_by.as_deref()) {
            warn!(
                publisher = incoming.published_by.as_deref().unwrap_or("unverified"),
                "dropped a message on {named} that is not the state agent's"
            );
            return;
        }
        if let Err(why) = soft_state_common::deliver(&router, &incoming.payload) {
            warn!("dropped a message on {named}: {why}");
        }
    })
}

/// One question, asked until it is answered or the attempts are spent, each attempt with a fresh correlation id.
pub async fn ask_until_answered(
    router: &ReplyRouter<StateReply>,
    query: &StateQuery,
    settings: StateSettings,
    publish: impl Fn(Vec<u8>) -> Result<(), String>,
) -> Result<SoftValue, StateError> {
    let mut last = String::new();
    for attempt in 1..=settings.attempts {
        let answer = soft_state_common::ask(
            router,
            query.wire(),
            || format!("{:032x}", rand::random::<u128>()),
            settings.reply_timeout,
            &publish,
        )
        .await;
        match answer {
            Answer::NoReply(why) => {
                debug!("attempt {attempt} of {}: {why}", settings.attempts);
                last = why;
            }
            answered => return outcome(answered),
        }
    }
    Err(StateError::NoReply(last))
}

pub struct StateArgs {
    pub client: TcpClient,
    pub router: Arc<ReplyRouter<StateReply>>,
    pub settings: StateSettings,
}

pub struct StateActor;

#[ractor::async_trait]
impl Actor for StateActor {
    type Msg = StateMessage;
    type State = StateArgs;
    type Arguments = StateArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: StateArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(args)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let StateMessage::Get { query, reply } = message;
        // Each question waits in a task of its own, so one that waits does not hold up the next.
        let router = state.router.clone();
        let client = state.client.clone();
        let settings = state.settings;
        tokio::spawn(async move {
            let result = ask_until_answered(&router, &query, settings, |payload| {
                client
                    .publish(PublishRequest {
                        topic: soft_state_common::REQUESTS_TOPIC.to_string(),
                        payload,
                        trace_ctx: WireTraceCtx::from_current_span(),
                        offline_behavior: OfflineBehavior::default(),
                        share_identity: true,
                    })
                    .map_err(|e| format!("{e:?}"))
            })
            .await;
            let _ = reply.send(result);
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cassini_client::TcpClientMessage;
    use soft_state_common::{Envelope, decode_request, encode_reply};
    use std::sync::Mutex;

    /// Each publish: its topic and its payload.
    type Sent = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

    /// Stands in for the broker client: keeps every publish, and answers the `answer_on`th question (counting from
    /// one) on the state agent's behalf, through `deliver`, as the agent's reply subscription would.
    struct Broker {
        sent: Sent,
        answer_on: usize,
        reply: StateReply,
        publisher: Option<String>,
        deliver: Arc<dyn Fn(polar_router::IncomingMessage) + Send + Sync>,
    }

    #[ractor::async_trait]
    impl Actor for Broker {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();
        async fn pre_start(&self, _: ActorRef<Self::Msg>, _: ()) -> Result<(), ActorProcessingErr> {
            Ok(())
        }
        async fn handle(
            &self,
            _: ActorRef<Self::Msg>,
            message: Self::Msg,
            _: &mut (),
        ) -> Result<(), ActorProcessingErr> {
            if let TcpClientMessage::Publish { topic, payload, .. } = message {
                let count = {
                    let mut sent = self.sent.lock().unwrap();
                    sent.push((topic, payload.clone()));
                    sent.len()
                };
                if count == self.answer_on {
                    let asked = decode_request(&payload).unwrap();
                    (self.deliver)(polar_router::IncomingMessage {
                        topic: soft_state_common::replies_topic("asker"),
                        payload: encode_reply(&Envelope {
                            correlation_id: asked.correlation_id,
                            payload: self.reply.clone(),
                        }),
                        published_by: self.publisher.clone(),
                        trace_ctx: None,
                    });
                }
            }
            Ok(())
        }
    }

    const STATE_AGENT: &str = "soft-state-observer.polar.serviceaccount.cluster.local";

    fn max() -> StateQuery {
        StateQuery::Max {
            label: "JiraIssue".into(),
            property: "updated_ms".into(),
        }
    }

    /// A state actor whose broker answers the `answer_on`th question with `reply`, as `publisher`.
    async fn actor(
        answer_on: usize,
        reply: StateReply,
        publisher: Option<&str>,
        attempts: u64,
    ) -> (StateHandle, Sent) {
        let router = Arc::new(ReplyRouter::new());
        let subscription = replies_subscription("asker", STATE_AGENT.to_string(), router.clone());
        assert_eq!(subscription.topic, "soft-state-replies.asker");
        let deliver: Arc<dyn Fn(polar_router::IncomingMessage) + Send + Sync> =
            Arc::from(subscription.deliver);
        let sent: Sent = Arc::new(Mutex::new(Vec::new()));
        let (broker, _) = Actor::spawn(
            None,
            Broker {
                sent: sent.clone(),
                answer_on,
                reply,
                publisher: publisher.map(str::to_string),
                deliver,
            },
            (),
        )
        .await
        .unwrap();
        let (state, _) = Actor::spawn(
            None,
            StateActor,
            StateArgs {
                client: TcpClient::from_actor_ref(broker),
                router,
                settings: StateSettings {
                    reply_timeout: Duration::from_millis(100),
                    attempts,
                },
            },
        )
        .await
        .unwrap();
        (StateHandle::new(state), sent)
    }

    fn ids(sent: &Sent) -> Vec<String> {
        sent.lock()
            .unwrap()
            .iter()
            .map(|(topic, payload)| {
                assert_eq!(topic, soft_state_common::REQUESTS_TOPIC);
                let asked = decode_request(payload).unwrap();
                assert_eq!(
                    asked.payload,
                    StateRequest::Max {
                        label: "JiraIssue".into(),
                        property: "updated_ms".into()
                    }
                );
                asked.correlation_id
            })
            .collect()
    }

    #[tokio::test]
    async fn a_question_is_asked_again_with_a_fresh_id_until_the_state_agent_answers() {
        let (state, sent) = actor(2, StateReply::Value(Some(42)), Some(STATE_AGENT), 3).await;
        assert_eq!(state.get(max()).await, Ok(SoftValue::of(Some(42))));
        let ids = ids(&sent);
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
    }

    #[tokio::test]
    async fn an_answer_is_never_asked_again() {
        for reply in [
            StateReply::Refused("not declared".into()),
            StateReply::Unavailable("graph down".into()),
            StateReply::Value(None),
        ] {
            let (state, sent) = actor(1, reply.clone(), Some(STATE_AGENT), 3).await;
            let got = state.get(max()).await;
            assert_eq!(got, outcome(Answer::Replied(reply)));
            assert_eq!(sent.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn a_reply_from_anyone_but_the_state_agent_is_not_an_answer() {
        for publisher in [
            Some("jira-processor.polar.serviceaccount.cluster.local"),
            Some("soft-state-observer.elsewhere.serviceaccount.cluster.local"),
            Some("soft-state-observer"),
            None,
        ] {
            let (state, sent) = actor(1, StateReply::Value(Some(1)), publisher, 2).await;
            assert!(
                matches!(state.get(max()).await, Err(StateError::NoReply(_))),
                "{publisher:?}"
            );
            // Both attempts were made, and nothing believed.
            assert_eq!(sent.lock().unwrap().len(), 2, "{publisher:?}");
        }
    }

    #[test]
    fn only_the_state_agent_of_the_agent_s_own_namespace_and_domain_is_believed() {
        let expected =
            state_agent_identity("jira-observer.polar.serviceaccount.cluster.local").unwrap();
        assert_eq!(expected, STATE_AGENT);
        assert!(from_state_agent(&expected, Some(STATE_AGENT)));
        for other in [
            "soft-state-observer.other.serviceaccount.cluster.local",
            "soft-state-observer.polar.serviceaccount.example.org",
            "jira-observer.polar.serviceaccount.cluster.local",
            "soft-state-observer",
        ] {
            assert!(!from_state_agent(&expected, Some(other)), "{other}");
        }
        assert!(!from_state_agent(&expected, None));
        assert_eq!(state_agent_identity("2f0c6a1e-alice"), None);
    }

    #[test]
    fn the_settings_default_to_eight_seconds_and_three_attempts_and_refuse_zero() {
        let none = |_: &str| None;
        assert_eq!(
            StateSettings::from_env(&none).unwrap(),
            StateSettings {
                reply_timeout: Duration::from_secs(8),
                attempts: 3
            }
        );
        let zero = |name: &str| (name == "POLAR_STATE_ATTEMPTS").then(|| "0".to_string());
        assert_eq!(relations(&zero).len(), 1);
        assert!(relations(&none).is_empty());
    }
}
