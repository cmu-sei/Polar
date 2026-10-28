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

//! The credentials agent's request gate (docs/architecture/authority-model.md, slice 9): every request on one of its
//! request topics is judged before its handler sees it. Cedar decides whether the broker-verified publisher may make the
//! request (`Action::"request"` on `Request::"<topic>"`), with the publisher's groups from its certificate in the
//! ledger; where policy is silent, the floor admits the topic's own publishers in the authority model. The rule is the
//! model's own (`credentials_agent_model::requests::admitted`), so what the gate admits is what the judge allows.
//!
//! One actor, so requests keep the order they arrived in. A refused request is dropped with a warning and gets no
//! reply: it is not recorded in the ledger, since anyone who can reach a topic could cause one without bound.

use crate::ledger::LedgerMessage;
use crate::record_asker::RecordAskerMessage;
use credentials_agent_model::Model;
use credentials_agent_model::requests::{admitted, facts_of, is_request, publishes};
use polar_agent_builder::topic_gate::PolicySaid;
use polar_agent_builder::{LookupError, PolicyLookup};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::sync::Arc;
use tracing::warn;

/// Where an admitted request goes: the supervisor's dispatch, in the agent.
pub type Dispatch = Arc<dyn Fn(String, Vec<u8>, Option<String>) + Send + Sync>;

pub enum RequestGateMessage {
    /// A request, as the broker delivered it.
    Request {
        topic: String,
        payload: Vec<u8>,
        published_by: Option<String>,
    },
}

pub struct RequestGateArgs {
    pub ledger: ActorRef<LedgerMessage>,
    pub policy: PolicyLookup,
    /// The authority model, whose publishers are the floor; none admits no one but whom a policy permits.
    pub model: Option<Arc<Model>>,
    /// Told to ask cert-issuer for its record when a requester's certificate is not in the ledger yet.
    pub record_asker: Option<ActorRef<RecordAskerMessage>>,
    pub dispatch: Dispatch,
}

pub struct RequestGate;

/// Whether the gate admits a request on `topic` from `published_by`, with the reason when it does not.
pub async fn judged(
    args: &RequestGateArgs,
    topic: &str,
    published_by: Option<&str>,
) -> Result<(), String> {
    if !is_request(topic) {
        return Err(format!(
            "`{topic}` is not one of the credentials agent's request topics"
        ));
    }
    let Some(requester) = published_by else {
        return Err(format!("a request on `{topic}` with no verified publisher"));
    };
    // Its groups are its certificate's, as the broker and every agent read them: its workload's, or its account's.
    let certificate = match ractor::call!(args.ledger, |reply| {
        LedgerMessage::RequesterCertificate {
            identity: requester.to_string(),
            reply,
        }
    }) {
        Ok(Some(pem)) => pem,
        Ok(None) => {
            // Signed by cert-issuer since the record was last read, perhaps: ask for it now, for the next try.
            if let Some(asker) = &args.record_asker {
                let _ = asker.send_message(RecordAskerMessage::Ask);
            }
            return Err(format!(
                "{requester} has no certificate the ledger holds, so its request on `{topic}` cannot be judged"
            ));
        }
        Err(e) => {
            return Err(format!(
                "the ledger could not be asked for {requester}'s certificate: {e}"
            ));
        }
    };
    let origin = args
        .model
        .as_deref()
        .and_then(|model| publishes(model, requester, topic));
    let said = if args.policy.actor().is_none() {
        PolicySaid::NotRunning
    } else {
        match args
            .policy
            .decide(
                requester,
                Some(certificate),
                r#"Action::"request""#,
                &format!(r#"Request::"{topic}""#),
                polar_policy::Facts {
                    topic: Some(
                        if credentials_agent_model::requests::custodian_publishes(requester, topic)
                        {
                            polar_policy::TopicFacts {
                                declared: false,
                                implied: true,
                            }
                        } else {
                            facts_of(origin)
                        },
                    ),
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
    admitted(
        &said,
        origin.is_some()
            || credentials_agent_model::requests::custodian_publishes(requester, topic),
        topic,
    )
}

#[ractor::async_trait]
impl Actor for RequestGate {
    type Msg = RequestGateMessage;
    type State = RequestGateArgs;
    type Arguments = RequestGateArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: RequestGateArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(args)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let RequestGateMessage::Request {
            topic,
            payload,
            published_by,
        } = message;
        match judged(state, &topic, published_by.as_deref()).await {
            Ok(()) => (state.dispatch)(topic, payload, published_by),
            Err(why) => warn!(
                %topic,
                requester = published_by.as_deref().unwrap_or("unverified"),
                "a request was refused at the credentials agent's request gate: {why}"
            ),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieve_secret::tests::{registered_policy, spawn_test_ledger};
    use cert_issuer_common::record::{IssuePath, RecordEntry, RecordReply};
    use credentials_agent_common::{
        CREDENTIALS_ISSUANCE_REQUESTS_TOPIC, CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
        CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC,
    };
    use std::sync::Mutex;
    use std::time::Duration;

    const MODEL: &str = include_str!("../../model/tests/fixtures/sandbox-model.json");

    fn id(workload: &str) -> String {
        format!("{workload}.polar.serviceaccount.cluster.local")
    }

    /// A client certificate naming `identity`, carrying `groups` as `g:` names, as cert-issuer and the credentials agent
    /// write them.
    fn certificate(identity: &str, groups: &[&str]) -> String {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec![identity.to_string()]).unwrap();
        for g in groups {
            params
                .subject_alt_names
                .push(rcgen::SanType::URI(format!("g:{g}").try_into().unwrap()));
        }
        params.self_signed(&key).unwrap().pem()
    }

    /// The ledger told of `identity`'s certificate as cert-issuer's record reports it: the way a bootstrap service's
    /// certificate reaches it (docs/architecture/cert-issuer-isolation.md, "Step 2").
    async fn reported(ledger: &ActorRef<LedgerMessage>, seq: u64, identity: &str, groups: &[&str]) {
        let page = RecordReply {
            log: "log-1".to_string(),
            entries: vec![RecordEntry {
                log: "log-1".to_string(),
                seq,
                serial: format!("serial-{seq}"),
                identity: identity.to_string(),
                cert_type: cert_issuer_common::CertType::Client,
                path: IssuePath::TokenOnly,
                instance_binding: None,
                signed_at: 1000,
                not_after: 5000,
                certificate_pem: certificate(identity, groups),
            }],
            tickets: Vec::new(),
            more: false,
        };
        ractor::call!(ledger, |reply| LedgerMessage::TakeRecordPage {
            page,
            bootstrap: None,
            reply
        })
        .unwrap()
        .unwrap();
    }

    /// An asker stand-in that counts the asks.
    struct Asks(Arc<Mutex<usize>>);

    #[ractor::async_trait]
    impl Actor for Asks {
        type Msg = RecordAskerMessage;
        type State = ();
        type Arguments = ();
        async fn pre_start(&self, _: ActorRef<Self::Msg>, _: ()) -> Result<(), ActorProcessingErr> {
            Ok(())
        }
        async fn handle(
            &self,
            _: ActorRef<Self::Msg>,
            m: Self::Msg,
            _: &mut (),
        ) -> Result<(), ActorProcessingErr> {
            if matches!(m, RecordAskerMessage::Ask) {
                *self.0.lock().unwrap() += 1;
            }
            Ok(())
        }
    }

    /// What reached the dispatch: each request's topic, payload and publisher.
    type Dispatched = Arc<Mutex<Vec<(String, Vec<u8>, Option<String>)>>>;

    struct Rig {
        args: RequestGateArgs,
        asks: Arc<Mutex<usize>>,
        dispatched: Dispatched,
    }

    enum Policy<'a> {
        NoRules,
        Rules(&'a str),
        NotRunning,
    }

    /// A gate over the sandbox's model, a ledger holding the observer's and kube-consumer's certificates as cert-issuer's
    /// record reported them (the observer's in group `operators-relay`), and the policy given.
    async fn rig(policy: Policy<'_>, with_model: bool) -> Rig {
        let ledger = spawn_test_ledger().await;
        reported(
            &ledger,
            1,
            &id("credentials-agent-observer"),
            &["cassini-client", "operators-relay"],
        )
        .await;
        reported(
            &ledger,
            2,
            &id("kube-consumer"),
            &["cassini-client", "encryption"],
        )
        .await;
        reported(&ledger, 3, &id("kube-observer"), &["cassini-client"]).await;
        let policy = match policy {
            Policy::NoRules => registered_policy(None).await,
            Policy::Rules(source) => registered_policy(Some(source.to_string())).await,
            Policy::NotRunning => PolicyLookup::named("no-such-policy-actor"),
        };
        let asks = Arc::new(Mutex::new(0));
        let (asker, _) = Actor::spawn(None, Asks(asks.clone()), ()).await.unwrap();
        let dispatched = Arc::new(Mutex::new(Vec::new()));
        let sink = dispatched.clone();
        Rig {
            args: RequestGateArgs {
                ledger,
                policy,
                model: with_model.then(|| Arc::new(Model::parse(MODEL).unwrap())),
                record_asker: Some(asker),
                dispatch: Arc::new(move |topic, payload, by| {
                    sink.lock().unwrap().push((topic, payload, by))
                }),
            },
            asks,
            dispatched,
        }
    }

    async fn gate(r: &Rig, topic: &str, by: &str) -> Result<(), String> {
        judged(&r.args, topic, Some(by)).await
    }

    #[tokio::test]
    async fn with_no_rules_a_topic_s_own_publishers_are_admitted_by_any_origin_and_no_one_else() {
        let r = rig(Policy::NoRules, true).await;
        // Declared: the observer relays polar-init's issuance.
        gate(
            &r,
            CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC,
            &id("credentials-agent-observer"),
        )
        .await
        .unwrap();
        // Implied: an encryption agent stores a secret.
        gate(
            &r,
            CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
            &id("kube-consumer"),
        )
        .await
        .unwrap();
        // Neither publishes the other's.
        let refused = gate(
            &r,
            CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
            &id("credentials-agent-observer"),
        )
        .await
        .unwrap_err();
        assert!(refused.contains("does not publish"), "{refused}");
        assert!(
            gate(
                &r,
                CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC,
                &id("kube-observer")
            )
            .await
            .is_err()
        );
    }

    /// A custodian is no catalog workload: what it may publish is its identity's, and nothing more
    /// (docs/architecture/key-custody.md, "Part A's bootstrap: the custodian").
    #[tokio::test]
    async fn with_no_rules_a_custodian_is_admitted_on_its_own_requests_and_no_others() {
        use crate::custodian::tests::{CUSTODIAN, POD, custodian_pem};
        let r = rig(Policy::NoRules, true).await;
        crate::issuance::call_append(
            &r.args.ledger,
            credentials_agent_common::events::CredentialsEvent::IssuanceCompleted {
                identity: CUSTODIAN.to_string(),
                instance_binding: POD.to_string(),
                serial: "0c".to_string(),
                timestamp: polar::time::now_millis(),
                cert_type: credentials_agent_common::issuance::WorkloadCertType::Custody,
                grant_key: None,
                certificate_pem: custodian_pem(
                    &cert_issuer_common::custody::CustodyPermission::ALL,
                    3600,
                ),
            },
        )
        .await
        .unwrap();
        for topic in credentials_agent_common::CUSTODIAN_PUBLISHES
            .into_iter()
            .filter(|t| credentials_agent_common::REQUEST_TOPICS.contains(t))
        {
            gate(&r, topic, CUSTODIAN).await.unwrap();
        }
        for topic in credentials_agent_common::REQUEST_TOPICS
            .into_iter()
            .filter(|t| !credentials_agent_common::CUSTODIAN_PUBLISHES.contains(t))
        {
            let refused = gate(&r, topic, CUSTODIAN).await.unwrap_err();
            assert!(refused.contains("does not publish"), "{topic}: {refused}");
        }
        // And a workload does not publish its custodian's: kube-observer has no encryption to ask with.
        let refused = gate(
            &r,
            credentials_agent_common::CREDENTIALS_CUSTODIAN_ISSUANCE_REQUESTS_TOPIC,
            &id("kube-observer"),
        )
        .await
        .unwrap_err();
        assert!(refused.contains("does not publish"), "{refused}");
    }

    #[tokio::test]
    async fn a_permit_admits_who_the_floor_does_not_and_a_forbid_refuses_who_it_does() {
        let policy = format!(
            r#"permit (principal == Identity::"{kube}", action == Action::"request", resource == Request::"{CREDENTIALS_ISSUANCE_REQUESTS_TOPIC}");
               forbid (principal == Identity::"{observer}", action == Action::"request", resource == Request::"{CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC}");"#,
            kube = id("kube-observer"),
            observer = id("credentials-agent-observer"),
        );
        let r = rig(Policy::Rules(&policy), true).await;
        gate(
            &r,
            CREDENTIALS_ISSUANCE_REQUESTS_TOPIC,
            &id("kube-observer"),
        )
        .await
        .unwrap();
        let refused = gate(
            &r,
            CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC,
            &id("credentials-agent-observer"),
        )
        .await
        .unwrap_err();
        assert!(refused.contains("policy forbids"), "{refused}");
        // Where the policy is silent, the floor still decides.
        gate(
            &r,
            CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
            &id("kube-consumer"),
        )
        .await
        .unwrap();
    }

    /// The groups policy sees are the requester's certificate's, from the ledger: here a bootstrap service's, which only
    /// cert-issuer's record told the ledger of.
    #[tokio::test]
    async fn a_group_rule_applies_through_the_certificate_the_ledger_holds() {
        let policy = r#"forbid (principal in Group::"operators-relay", action == Action::"request", resource);"#;
        let r = rig(Policy::Rules(policy), true).await;
        let refused = gate(
            &r,
            CREDENTIALS_ISSUANCE_REQUESTS_TOPIC,
            &id("credentials-agent-observer"),
        )
        .await
        .unwrap_err();
        assert!(refused.contains("policy forbids"), "{refused}");
        gate(
            &r,
            CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
            &id("kube-consumer"),
        )
        .await
        .unwrap();
    }

    /// A person publishes no request topic, so only a permit admits one; its groups are its account certificate's.
    #[tokio::test]
    async fn a_person_is_admitted_by_a_permit_alone_through_their_account_certificate() {
        let account = "4f0c1d7e-0000-4000-8000-000000000001";
        let policy = r#"permit (principal in Group::"operators", action == Action::"request", resource == Request::"credentials-certificate-requests");"#;
        let r = rig(Policy::Rules(policy), true).await;
        ractor::call!(r.args.ledger, |reply| LedgerMessage::Append {
            event: credentials_agent_common::events::CredentialsEvent::AccountCertificateIssued {
                account_id: account.to_string(),
                username: "alice".to_string(),
                serial: "alice-1".to_string(),
                timestamp: 1000,
                certificate_pem: certificate(account, &["operators"]),
            },
            reply
        })
        .unwrap()
        .unwrap();
        gate(&r, "credentials-certificate-requests", account)
            .await
            .unwrap();
        let refused = gate(&r, CREDENTIALS_ISSUANCE_REQUESTS_TOPIC, account)
            .await
            .unwrap_err();
        assert!(refused.contains("does not publish"), "{refused}");
    }

    #[tokio::test]
    async fn a_request_with_no_verified_publisher_or_no_certificate_on_record_is_refused_and_the_record_asked_for()
     {
        let r = rig(Policy::NoRules, true).await;
        let refused = judged(&r.args, CREDENTIALS_ISSUANCE_REQUESTS_TOPIC, None)
            .await
            .unwrap_err();
        assert!(refused.contains("no verified publisher"), "{refused}");
        assert_eq!(*r.asks.lock().unwrap(), 0);
        // A publisher of the topic, whose certificate cert-issuer has signed and the ledger not yet heard of.
        let refused = gate(
            &r,
            CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
            &id("git-repo-observer"),
        )
        .await
        .unwrap_err();
        assert!(refused.contains("no certificate"), "{refused}");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            *r.asks.lock().unwrap(),
            1,
            "cert-issuer's record is asked for at once"
        );
    }

    /// A policy actor that answers every question with an error: no decision.
    struct Failing;

    #[ractor::async_trait]
    impl Actor for Failing {
        type Msg = polar_policy::PolicyMessage;
        type State = ();
        type Arguments = ();
        async fn pre_start(&self, _: ActorRef<Self::Msg>, _: ()) -> Result<(), ActorProcessingErr> {
            Ok(())
        }
        async fn handle(
            &self,
            _: ActorRef<Self::Msg>,
            m: Self::Msg,
            _: &mut (),
        ) -> Result<(), ActorProcessingErr> {
            if let polar_policy::PolicyMessage::CheckAuthorization { reply, .. } = m {
                let _ = reply.send(Err(polar_policy::PolicyError::PermissionsResolution(
                    "down".to_string(),
                )));
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_policy_that_cannot_be_consulted_refuses_and_one_not_running_yet_leaves_the_floor() {
        // No answer is never a grant, even for the topic's own publisher.
        let mut r = rig(Policy::NotRunning, true).await;
        let (failing, _) = Actor::spawn(
            Some("request-gate-test-failing-policy".to_string()),
            Failing,
            (),
        )
        .await
        .unwrap();
        r.args.policy = PolicyLookup::named("request-gate-test-failing-policy");
        let refused = gate(
            &r,
            CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC,
            &id("credentials-agent-observer"),
        )
        .await
        .unwrap_err();
        assert!(refused.contains("could not be consulted"), "{refused}");
        failing.stop(None);
        // Not running at all: the floor decides, as the topic gate's rule says.
        let r = rig(Policy::NotRunning, true).await;
        gate(
            &r,
            CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC,
            &id("credentials-agent-observer"),
        )
        .await
        .unwrap();
        assert!(
            gate(
                &r,
                CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
                &id("credentials-agent-observer")
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn with_no_model_no_one_is_admitted_but_whom_a_policy_permits() {
        let r = rig(Policy::NoRules, false).await;
        assert!(
            gate(
                &r,
                CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC,
                &id("credentials-agent-observer")
            )
            .await
            .is_err()
        );
        let policy = format!(
            r#"permit (principal == Identity::"{}", action == Action::"request", resource);"#,
            id("credentials-agent-observer")
        );
        let r = rig(Policy::Rules(&policy), false).await;
        gate(
            &r,
            CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC,
            &id("credentials-agent-observer"),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_topic_that_is_no_request_topic_is_refused() {
        let r = rig(
            Policy::Rules(r#"permit (principal, action, resource);"#),
            true,
        )
        .await;
        let refused = gate(
            &r,
            "polar-authority-updates",
            &id("credentials-agent-observer"),
        )
        .await
        .unwrap_err();
        assert!(
            refused.contains("not one of the credentials agent's request topics"),
            "{refused}"
        );
    }

    /// Admitted requests reach the dispatch in the order they arrived, publisher and all; a refused one never does.
    #[tokio::test]
    async fn the_gate_dispatches_what_it_admits_in_order_and_nothing_it_refuses() {
        let r = rig(Policy::NoRules, true).await;
        let dispatched = r.dispatched.clone();
        let (gate, _) = Actor::spawn(None, RequestGate, r.args).await.unwrap();
        let observer = Some(id("credentials-agent-observer"));
        for (topic, payload, by) in [
            (
                CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC,
                vec![1],
                observer.clone(),
            ),
            (
                CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
                vec![2],
                observer.clone(),
            ),
            (
                CREDENTIALS_ISSUANCE_REQUESTS_TOPIC,
                vec![3],
                observer.clone(),
            ),
            (CREDENTIALS_ISSUANCE_REQUESTS_TOPIC, vec![4], None),
        ] {
            gate.send_message(RequestGateMessage::Request {
                topic: topic.to_string(),
                payload,
                published_by: by,
            })
            .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            dispatched.lock().unwrap().clone(),
            vec![
                (
                    CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC.to_string(),
                    vec![1],
                    observer.clone()
                ),
                (
                    CREDENTIALS_ISSUANCE_REQUESTS_TOPIC.to_string(),
                    vec![3],
                    observer.clone()
                ),
            ]
        );
    }

    mod agreement {
        //! The gate admits exactly what the judge allows: for any policy, every catalog workload and a person, on every
        //! request topic. The judge reads a workload's groups from the model and the gate from its certificate, which
        //! carries the same ones; the decision is one rule.

        use super::*;
        use credentials_agent_model::{Action, Pair, Person, allowed_under};
        use proptest::prelude::*;
        use proptest::test_runner::{Config, TestCaseError, TestRunner};

        const ALICE: &str = "4f0c1d7e-0000-4000-8000-000000000001";
        const GROUPS: [&str; 4] = ["cassini-client", "encryption", "self-test", "operators"];

        /// One rule: its effect, whom it names (anyone, a workload by index, a group by index), what (any request, or one
        /// topic by index), and its condition.
        #[derive(Debug, Clone)]
        struct Rule {
            permit: bool,
            principal: (u8, usize),
            resource: Option<usize>,
            condition: u8,
        }

        fn rules() -> impl Strategy<Value = Vec<Rule>> {
            prop::collection::vec(
                (
                    any::<bool>(),
                    (0u8..3, 0usize..64),
                    prop::option::of(0usize..13),
                    0u8..4,
                )
                    .prop_map(|(permit, principal, resource, condition)| Rule {
                        permit,
                        principal,
                        resource,
                        condition,
                    }),
                0..4,
            )
        }

        fn source(model: &Model, rules: &[Rule]) -> Option<String> {
            if rules.is_empty() {
                return None;
            }
            Some(
                rules
                    .iter()
                    .map(|r| {
                        let principal = match r.principal {
                            (0, _) => "principal".to_string(),
                            (1, w) => format!(
                                r#"principal == Identity::"{}""#,
                                model.workloads[w % model.workloads.len()].identity
                            ),
                            (_, g) => format!(r#"principal in Group::"{}""#, GROUPS[g % GROUPS.len()]),
                        };
                        let resource = match r.resource {
                            None => "resource".to_string(),
                            Some(t) => format!(
                                r#"resource == Request::"{}""#,
                                credentials_agent_common::REQUEST_TOPICS[t]
                            ),
                        };
                        let condition = match r.condition {
                            0 => "",
                            1 => " when { context.declared }",
                            2 => " when { context.implied }",
                            _ => " unless { context.declared }",
                        };
                        format!(
                            r#"{} ({principal}, action == Action::"request", {resource}){condition};"#,
                            if r.permit { "permit" } else { "forbid" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }

        #[test]
        fn the_gate_admits_exactly_what_the_judge_allows() {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let model = Arc::new(Model::parse(MODEL).unwrap());
            let alice = Person {
                identity: ALICE.to_string(),
                groups: vec!["operators".to_string()],
                capabilities: Vec::new(),
            };
            // Every workload's certificate as cert-issuer's record or the credentials agent's issuance gives it, with the
            // groups the catalog gives it; and Alice's account certificate.
            let ledger = runtime.block_on(async {
                let ledger = spawn_test_ledger().await;
                for (seq, w) in model.workloads.iter().enumerate() {
                    let groups: Vec<&str> = w.groups.iter().map(String::as_str).collect();
                    reported(&ledger, seq as u64 + 1, &w.identity, &groups).await;
                }
                ractor::call!(ledger, |reply| LedgerMessage::Append {
                    event: credentials_agent_common::events::CredentialsEvent::AccountCertificateIssued {
                        account_id: ALICE.to_string(),
                        username: "alice".to_string(),
                        serial: "alice-1".to_string(),
                        timestamp: 1000,
                        certificate_pem: certificate(ALICE, &["operators"]),
                    },
                    reply
                })
                .unwrap()
                .unwrap();
                ledger
            });
            let requesters: Vec<String> = model
                .workloads
                .iter()
                .map(|w| w.identity.clone())
                .chain([ALICE.to_string()])
                .collect();

            let mut runner = TestRunner::new(Config {
                cases: 24,
                ..Config::default()
            });
            runner
                .run(&rules(), |rules| {
                    let policy = source(&model, &rules);
                    let pair = Pair {
                        policy: policy.clone(),
                        memberships: Default::default(),
                        hierarchy: Default::default(),
                    };
                    let allowed = allowed_under(&model, &pair, std::slice::from_ref(&alice))
                        .map_err(TestCaseError::fail)?;
                    runtime.block_on(async {
                        let args = RequestGateArgs {
                            ledger: ledger.clone(),
                            policy: registered_policy(policy.clone()).await,
                            model: Some(model.clone()),
                            record_asker: None,
                            dispatch: Arc::new(|_, _, _| {}),
                        };
                        for requester in &requesters {
                            for topic in credentials_agent_common::REQUEST_TOPICS {
                                let gate = judged(&args, topic, Some(requester)).await.is_ok();
                                let judge = allowed.contains(&(
                                    requester.clone(),
                                    topic.to_string(),
                                    Action::Request,
                                ));
                                prop_assert_eq!(
                                    gate,
                                    judge,
                                    "{} on {} under {:?}",
                                    requester,
                                    topic,
                                    policy
                                );
                            }
                        }
                        Ok(())
                    })
                })
                .unwrap();
        }
    }
}
