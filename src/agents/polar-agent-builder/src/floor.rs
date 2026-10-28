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

//! What a client's certificate admits where no policy speaks: the floor the broker decides on
//! (docs/architecture/policy-enforcement.md, "Decisions"). The declared topics are the certificate's `p:pub:` and
//! `p:sub:` names; the implied ones follow from the identity's workload and the certificate's `g:` groups, by
//! [`implied_topics`], the function the agent computes its own set with. So for an agent's own certificate the
//! floor and the agent's [`TopicSet`](crate::TopicSet) are the same set, told apart the same way.

use crate::topics::{Access, implied_topics};
use base64::Engine;
use cert_issuer_common::group::extract_groups;
use cert_issuer_common::identity::workload_of;
use cert_issuer_common::permission::{
    TopicAccess, extract_own_identity, extract_topic_permissions,
};
use std::collections::BTreeSet;

/// One client's floor, read once from the certificate it connected with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Floor {
    identity: String,
    certificate_pem: String,
    declared: BTreeSet<(String, Access)>,
    implied: BTreeSet<(String, Access)>,
}

impl Floor {
    /// The floor of the certificate `certificate_pem`. Its identity is its first DNS name; one that names no
    /// workload (a person's) implies nothing, so only what it declares is admitted.
    pub fn from_certificate_pem(certificate_pem: &str) -> Result<Self, String> {
        let identity = extract_own_identity(certificate_pem).map_err(|e| e.to_string())?;
        let declared = extract_topic_permissions(certificate_pem)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|p| {
                let access = match p.access {
                    TopicAccess::Publish => Access::Publish,
                    TopicAccess::Subscribe => Access::Subscribe,
                };
                (p.topic, access)
            })
            .collect();
        let groups: Vec<String> = extract_groups(certificate_pem)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|g| g.0)
            .collect();
        let implied = match workload_of(&identity) {
            Some(workload) => implied_topics(workload, &groups)
                .into_iter()
                .map(|(key, _)| key)
                .collect(),
            // A custodian is no workload: what it may use is its identity's, the same for every custodian, and nothing
            // its certificate could add (docs/architecture/key-custody.md, "Part A's bootstrap: the custodian").
            None if cert_issuer_common::custody::custodian_of(&identity).is_some() => {
                custodian_topics()
            }
            None => BTreeSet::new(),
        };
        let declared = if cert_issuer_common::custody::custodian_of(&identity).is_some() {
            BTreeSet::new()
        } else {
            declared
        };
        Ok(Self {
            identity,
            certificate_pem: certificate_pem.to_string(),
            declared,
            implied,
        })
    }

    /// As [`from_certificate_pem`](Self::from_certificate_pem), from the certificate as the TLS handshake gives it.
    pub fn from_certificate_der(der: &[u8]) -> Result<Self, String> {
        let body = base64::engine::general_purpose::STANDARD.encode(der);
        let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
        for line in body.as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
        Self::from_certificate_pem(&pem)
    }

    /// The certificate's identity, which policy is asked about.
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// The certificate, for the groups it carries to count in policy's decisions.
    pub fn certificate_pem(&self) -> &str {
        &self.certificate_pem
    }

    /// Whether the certificate admits using `topic` this way.
    pub fn allows(&self, topic: &str, access: Access) -> bool {
        let key = (topic.to_string(), access);
        self.implied.contains(&key) || self.declared.contains(&key)
    }

    /// What policy is told about `topic`, used this way: as [`TopicSet::facts`](crate::TopicSet::facts) tells it,
    /// a topic both declared and implied being implied.
    pub fn facts(&self, topic: &str, access: Access) -> polar_policy::TopicFacts {
        let key = (topic.to_string(), access);
        let implied = self.implied.contains(&key);
        polar_policy::TopicFacts {
            declared: !implied && self.declared.contains(&key),
            implied,
        }
    }
}

/// What every custodian may publish and subscribe to: its own requests and their replies, and the authority.
fn custodian_topics() -> BTreeSet<(String, Access)> {
    use credentials_agent_common::{CUSTODIAN_PUBLISHES, CUSTODIAN_SUBSCRIBES};
    CUSTODIAN_PUBLISHES
        .iter()
        .map(|t| (t.to_string(), Access::Publish))
        .chain(
            CUSTODIAN_SUBSCRIBES
                .iter()
                .map(|t| (t.to_string(), Access::Subscribe)),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::own_name::tests::certificate_with;
    use crate::topics::{Facts, TopicDecl, TopicSet, groups_for};
    use proptest::prelude::*;

    const IDENTITY: &str = "kube-observer.polar.serviceaccount.cluster.local";

    #[test]
    fn the_declared_topics_are_the_certificate_s_and_the_implied_follow_from_its_groups() {
        let floor = Floor::from_certificate_pem(&certificate_with(&[
            IDENTITY,
            "p:pub:kubernetes.cluster.consumer.Pod",
            "g:cassini-client",
        ]))
        .unwrap();
        assert_eq!(floor.identity(), IDENTITY);
        assert!(floor.allows("kubernetes.cluster.consumer.Pod", Access::Publish));
        // Declared to publish is not declared to subscribe.
        assert!(!floor.allows("kubernetes.cluster.consumer.Pod", Access::Subscribe));
        // A Cassini client hears its own configuration, and nobody else's.
        assert!(floor.allows("agent.kube-observer.configuration", Access::Subscribe));
        assert!(!floor.allows("agent.kube-consumer.configuration", Access::Subscribe));
        // Not in the self-test group, so not its probe topic.
        assert!(!floor.allows(
            &crate::infrastructure::self_test_topic("kube-observer"),
            Access::Publish
        ));
        assert_eq!(
            floor.facts("kubernetes.cluster.consumer.Pod", Access::Publish),
            polar_policy::TopicFacts {
                declared: true,
                implied: false
            }
        );
        assert_eq!(
            floor.facts("agent.kube-observer.configuration", Access::Subscribe),
            polar_policy::TopicFacts {
                declared: false,
                implied: true
            }
        );
        assert_eq!(
            floor.facts("somebody.elses", Access::Publish),
            polar_policy::TopicFacts::default()
        );
    }

    #[test]
    fn a_custodian_may_use_its_own_requests_and_the_authority_and_nothing_its_certificate_adds() {
        let custodian = "custody.kube-consumer.polar.serviceaccount.cluster.local";
        let floor = Floor::from_certificate_pem(&certificate_with(&[
            custodian,
            "p:custody:client",
            "p:pub:kubernetes.cluster.consumer.Pod",
            "g:cassini-client",
        ]))
        .unwrap();
        for topic in credentials_agent_common::CUSTODIAN_PUBLISHES {
            assert!(floor.allows(topic, Access::Publish), "{topic}");
            assert!(floor.facts(topic, Access::Publish).implied, "{topic}");
        }
        for topic in credentials_agent_common::CUSTODIAN_SUBSCRIBES {
            assert!(floor.allows(topic, Access::Subscribe), "{topic}");
        }
        // Not what its certificate declares, not its group's, not its workload's.
        assert!(!floor.allows("kubernetes.cluster.consumer.Pod", Access::Publish));
        assert!(!floor.allows("agent.kube-consumer.configuration", Access::Subscribe));
        assert!(!floor.allows(
            credentials_agent_common::CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC,
            Access::Publish
        ));
        // And its workload is not given its custodian's.
        let workload = Floor::from_certificate_pem(&certificate_with(&[
            "kube-consumer.polar.serviceaccount.cluster.local",
            "g:cassini-client",
        ]))
        .unwrap();
        assert!(!workload.allows(
            credentials_agent_common::CREDENTIALS_CUSTODIAN_ISSUANCE_REQUESTS_TOPIC,
            Access::Publish
        ));
    }

    #[test]
    fn a_certificate_without_groups_implies_nothing() {
        let floor = Floor::from_certificate_pem(&certificate_with(&[IDENTITY])).unwrap();
        assert!(!floor.allows("agent.kube-observer.configuration", Access::Subscribe));
    }

    #[test]
    fn a_person_s_certificate_implies_nothing_whatever_groups_it_names() {
        let floor = Floor::from_certificate_pem(&certificate_with(&[
            "alice",
            "g:cassini-client",
            "p:sub:news",
        ]))
        .unwrap();
        assert_eq!(floor.identity(), "alice");
        assert!(floor.allows("news", Access::Subscribe));
        assert!(!floor.allows("agent.alice.configuration", Access::Subscribe));
        assert!(!floor.allows("polar-authority-updates", Access::Subscribe));
    }

    #[test]
    fn the_floor_reads_the_same_from_the_handshake_s_der() {
        let pem = certificate_with(&[IDENTITY, "p:pub:x", "g:cassini-client"]);
        let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        let der = base64::engine::general_purpose::STANDARD
            .decode(body)
            .unwrap();
        let from_der = Floor::from_certificate_der(&der).unwrap();
        let from_pem = Floor::from_certificate_pem(&pem).unwrap();
        assert_eq!(from_der.identity(), from_pem.identity());
        assert_eq!(from_der.declared, from_pem.declared);
        assert_eq!(from_der.implied, from_pem.implied);
    }

    #[test]
    fn what_is_not_a_certificate_is_an_error() {
        assert!(Floor::from_certificate_pem("not a certificate").is_err());
        assert!(Floor::from_certificate_der(b"not a certificate either").is_err());
    }

    const TOPICS: &[&str] = &["a.one", "b.two", "c.three", "polar-authority-updates"];

    fn decls() -> impl Strategy<Value = Vec<TopicDecl>> {
        proptest::collection::vec(
            (proptest::sample::select(TOPICS), any::<bool>()).prop_map(|(name, publish)| {
                if publish {
                    TopicDecl::publish(name, "a purpose")
                } else {
                    TopicDecl::subscribe(name, "a purpose")
                }
            }),
            0..5,
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// The broker's floor of an agent's certificate (its declared topics as `p:` names, its groups as `g:`) is the
        /// agent's own set, told apart the same way: the agent and the broker cannot disagree about a topic.
        #[test]
        fn the_floor_of_an_agent_s_certificate_is_the_agent_s_own_set(
            cassini_client in any::<bool>(),
            self_test in any::<bool>(),
            encryption in any::<bool>(),
            declared in decls(),
            asked in proptest::sample::select(TOPICS),
            publish in any::<bool>(),
        ) {
            let facts = Facts { name: "kube-observer", cassini_client, self_test, encryption };
            let set = TopicSet::compute(&facts, &declared);
            let mut names: Vec<String> = vec![IDENTITY.to_string()];
            names.extend(declared.iter().map(|d| match d.access {
                Access::Publish => format!("p:pub:{}", d.name),
                Access::Subscribe => format!("p:sub:{}", d.name),
            }));
            names.extend(groups_for(&facts).iter().map(|g| format!("g:{g}")));
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let floor = Floor::from_certificate_pem(&certificate_with(&names)).unwrap();

            for entry in set.entries() {
                prop_assert!(floor.allows(&entry.name, entry.access), "{entry:?}");
                prop_assert_eq!(floor.facts(&entry.name, entry.access), set.facts(&entry.name, entry.access));
            }
            let access = if publish { Access::Publish } else { Access::Subscribe };
            prop_assert_eq!(floor.allows(asked, access), set.allows(asked, access));
            prop_assert_eq!(floor.facts(asked, access), set.facts(asked, access));
        }
    }
}
