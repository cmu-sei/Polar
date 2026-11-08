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

//! Pulled health, the asked side (docs/architecture/soft-state.md, "As built: pulled health"): every Cassini client
//! hears a question on its own topic, `agent.<name>.health` (implied by the `agent` rule), and the builder answers it
//! with the health actor's report on `polar-health-replies`. The domain is never asked; there is no heartbeat timer.
//!
//! Who may ask is decided as the credentials agent's request gate decides (`credentials-agent/consume/src/
//! request_gate.rs`): Cedar first, `Action::"request"` on `Request::"agent.<name>.health"` for the identity the broker
//! stamped (`published_by`); where policy is silent or none is loaded, the floor, which admits the state agent's writer
//! of the agent's own namespace and domain; a policy that cannot be consulted refuses. A refused question, or one with
//! no verified asker, is dropped with a warning and gets no answer.

use crate::topic_gate::PolicySaid;
use crate::{ConfigurationHandle, HealthHandle, LookupError, PolicyLookup, Subscription};
use cassini_client::{OfflineBehavior, PublishRequest};
use polar::cassini::{CassiniClient, TcpClient};
use polar_health::report::{self, HEALTH_REPLIES_TOPIC, HealthAnswer, HealthReport};
use ractor::ActorRef;
use tracing::{debug, warn};

/// The identity of the state agent's writer in the agent's own namespace and domain, `soft-state-processor.<namespace>.
/// serviceaccount.<domain>` for an agent whose identity is `<workload>.<namespace>.serviceaccount.<domain>`: whom the
/// floor lets ask. None for an identity that names no workload.
pub fn writer_identity(own_identity: &str) -> Option<String> {
    let workload = cert_issuer_common::identity::workload_of(own_identity)?;
    Some(format!(
        "{}{}",
        soft_state_common::PROCESSOR_NAME,
        &own_identity[workload.len()..]
    ))
}

/// Whether a question on `topic` is answered, given what policy said and whether the floor admits the asker: Cedar
/// first; where it is silent, or no policy is loaded or running, the floor; a policy that cannot be consulted refuses.
pub fn admitted(said: &PolicySaid, floor_admits: bool, topic: &str) -> Result<(), String> {
    let floor = || {
        if floor_admits {
            Ok(())
        } else {
            Err(format!(
                "the asker is not the state agent's writer, and no policy permits its question on `{topic}`"
            ))
        }
    };
    match said {
        PolicySaid::Decided(polar_policy::Decision::Allow) => Ok(()),
        PolicySaid::Decided(polar_policy::Decision::Deny) => {
            Err(format!("policy forbids the question on `{topic}`"))
        }
        PolicySaid::Decided(polar_policy::Decision::NotDetermined)
        | PolicySaid::NoPolicyLoaded
        | PolicySaid::NotRunning => floor(),
        PolicySaid::Unavailable(why) => Err(format!(
            "policy could not be consulted about a question on `{topic}` ({why}), so it is refused"
        )),
    }
}

/// The request gate: whether the question on `topic` from `published_by` is answered. `writer` is the identity the
/// floor admits (none: the floor admits nobody, and only a policy can).
pub async fn judged(
    policy: &PolicyLookup,
    writer: Option<&str>,
    topic: &str,
    published_by: Option<&str>,
) -> Result<(), String> {
    let Some(asker) = published_by else {
        return Err(format!("a question on `{topic}` with no verified asker"));
    };
    let floor_admits = writer == Some(asker);
    let said = if policy.actor().is_none() {
        PolicySaid::NotRunning
    } else {
        match policy
            .decide(
                asker,
                None,
                r#"Action::"request""#,
                &format!(r#"Request::"{topic}""#),
                polar_policy::Facts {
                    topic: Some(polar_policy::TopicFacts {
                        // The writer publishes the family it asks on: it declares the topic.
                        declared: floor_admits,
                        implied: false,
                    }),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(decision) => PolicySaid::Decided(decision),
            Err(LookupError::NoPolicyLoaded) => PolicySaid::NoPolicyLoaded,
            Err(LookupError::Unavailable(why)) => PolicySaid::Unavailable(why),
        }
    };
    admitted(&said, floor_admits, topic)
}

/// The report as it leaves the agent: its message cleaned of the agent's secret values in force, as well as of the
/// shapes credentials take.
pub fn outgoing(mut report: HealthReport, secrets: &[String]) -> HealthReport {
    let secrets: Vec<&str> = secrets.iter().map(String::as_str).collect();
    report.message = report
        .message
        .and_then(|message| report::sanitize(&message, &secrets));
    report
}

/// The subscription to the agent's own question topic: each question goes to the infrastructure supervisor, with the
/// identity the broker stamped on it.
pub fn subscription(
    service_name: &str,
    deliver_to: ActorRef<crate::InfrastructureSupervisorMessage>,
) -> Subscription {
    Subscription::new(report::health_topic(service_name), move |incoming| {
        if let Err(e) = deliver_to.cast(crate::InfrastructureSupervisorMessage::HealthQuery {
            payload: incoming.payload,
            published_by: incoming.published_by,
        }) {
            warn!("a health question could not be handed to the infrastructure: {e}");
        }
    })
}

/// What answering needs, owned, so a question is answered off the supervisor's own thread.
#[derive(Clone)]
pub(crate) struct Answerer {
    pub(crate) topic: String,
    pub(crate) writer: Option<String>,
    pub(crate) policy: PolicyLookup,
    pub(crate) health: HealthHandle,
    pub(crate) configuration: ConfigurationHandle,
    pub(crate) client: TcpClient,
}

impl Answerer {
    /// Judges the question, and answers it with the health actor's report if it is admitted.
    pub(crate) async fn answer(&self, payload: Vec<u8>, published_by: Option<String>) {
        let asker = published_by.as_deref().unwrap_or("unverified").to_string();
        let query = match report::decode_query(&payload) {
            Ok(query) => query,
            Err(why) => {
                warn!(requester = %asker, "dropped a message on {}: {why}", self.topic);
                return;
            }
        };
        if let Err(why) = judged(
            &self.policy,
            self.writer.as_deref(),
            &self.topic,
            published_by.as_deref(),
        )
        .await
        {
            warn!(requester = %asker, "a health question was refused: {why}");
            return;
        }
        let report = match self.health.report().await {
            Ok(report) => report,
            Err(why) => {
                warn!("a health question could not be answered: {why}");
                return;
            }
        };
        let secrets = match self.configuration.current().await {
            Ok(config) => config.secret_values(),
            Err(why) => {
                // Without the secrets in force the message cannot be cleaned of them, so it is not sent.
                warn!(
                    "the configuration could not be read, so the report goes without its message: {why}"
                );
                let mut report = report;
                report.message = None;
                return self.publish(query.correlation_id, report);
            }
        };
        self.publish(query.correlation_id, outgoing(report, &secrets));
    }

    fn publish(&self, correlation_id: String, report: HealthReport) {
        let payload = report::encode_answer(&HealthAnswer {
            correlation_id,
            report,
        });
        match self.client.publish(PublishRequest {
            topic: HEALTH_REPLIES_TOPIC.to_string(),
            payload,
            trace_ctx: None,
            // An answer that cannot go now is stale by the time it could: the asker counts it unresponsive.
            offline_behavior: OfflineBehavior::Drop,
            // Who answered is the broker's word, so the identity goes with it.
            share_identity: true,
        }) {
            Ok(()) => debug!("answered a health question"),
            Err(e) => warn!("could not answer a health question on {HEALTH_REPLIES_TOPIC}: {e:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar_health::{HealthPhase, HealthReason, ReportStatus};
    use polar_policy::Decision;
    use proptest::prelude::*;

    const OWN: &str = "jira-observer.polar.serviceaccount.cluster.local";
    const WRITER: &str = "soft-state-processor.polar.serviceaccount.cluster.local";

    #[test]
    fn the_floor_admits_the_writer_of_the_agent_s_own_namespace_and_domain() {
        assert_eq!(writer_identity(OWN).as_deref(), Some(WRITER));
        assert_eq!(
            writer_identity("kube-observer.other.serviceaccount.example.org").as_deref(),
            Some("soft-state-processor.other.serviceaccount.example.org")
        );
        assert_eq!(writer_identity("not an identity"), None);
    }

    fn every_answer() -> Vec<PolicySaid> {
        vec![
            PolicySaid::NotRunning,
            PolicySaid::Decided(Decision::Allow),
            PolicySaid::Decided(Decision::Deny),
            PolicySaid::Decided(Decision::NotDetermined),
            PolicySaid::NoPolicyLoaded,
            PolicySaid::Unavailable("down".to_string()),
        ]
    }

    /// Cedar decides when it speaks; where it is silent the floor does; a policy that cannot be consulted refuses.
    #[test]
    fn cedar_first_then_the_floor_and_an_unavailable_policy_refuses() {
        let topic = "agent.jira-observer.health";
        for said in every_answer() {
            for floor in [true, false] {
                let got = admitted(&said, floor, topic).is_ok();
                let want = match &said {
                    PolicySaid::Decided(Decision::Allow) => true,
                    PolicySaid::Decided(Decision::Deny) | PolicySaid::Unavailable(_) => false,
                    _ => floor,
                };
                assert_eq!(got, want, "{said:?} floor={floor}");
            }
        }
    }

    #[tokio::test]
    async fn a_question_with_no_verified_asker_or_from_anyone_but_the_writer_is_refused_without_a_policy()
     {
        let policy = PolicyLookup::named("no-such-policy-actor");
        let topic = "agent.jira-observer.health";
        assert!(judged(&policy, Some(WRITER), topic, None).await.is_err());
        assert!(
            judged(&policy, Some(WRITER), topic, Some(WRITER))
                .await
                .is_ok()
        );
        assert!(
            judged(
                &policy,
                Some(WRITER),
                topic,
                Some("kube-observer.polar.serviceaccount.cluster.local")
            )
            .await
            .is_err()
        );
        // The same workload name in another namespace is another identity.
        assert!(
            judged(
                &policy,
                Some(WRITER),
                topic,
                Some("soft-state-processor.elsewhere.serviceaccount.cluster.local")
            )
            .await
            .is_err()
        );
        assert!(judged(&policy, None, topic, Some(WRITER)).await.is_err());
    }

    fn report_with(message: Option<String>) -> HealthReport {
        HealthReport {
            status: ReportStatus::Unhealthy,
            phase: HealthPhase::Normal,
            reason: Some(HealthReason::Credentials),
            message,
            cassini_connected: true,
            graph_connected: false,
            expects_graph: false,
            counters: Default::default(),
            uptime_secs: 1,
        }
    }

    #[test]
    fn the_outgoing_message_is_cleaned_of_the_secrets_in_force() {
        let out = outgoing(
            report_with(Some("Jira refused the token tok-12345678".into())),
            &["tok-12345678".to_string()],
        );
        assert_eq!(
            out.message.as_deref(),
            Some("Jira refused the token <withheld>")
        );
        assert_eq!(outgoing(report_with(None), &[]).message, None);
    }

    proptest! {
        /// What leaves the agent never carries a secret in force.
        #[test]
        fn no_secret_in_force_leaves_in_a_report(
            secret in "[A-Za-z0-9_-]{4,20}",
            before in "\\PC{0,40}",
            after in "\\PC{0,40}",
        ) {
            let out = outgoing(report_with(Some(format!("{before}{secret}{after}"))), std::slice::from_ref(&secret));
            if let Some(message) = out.message {
                prop_assert!(!message.contains(&secret));
                prop_assert!(message.chars().count() <= polar_health::report::MAX_MESSAGE_CHARS);
            }
        }
    }
}
