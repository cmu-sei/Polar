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

//! The authority model's base facts, as the render writes them (`infra/rules.nu`, `model_facts`), and the pair of
//! documents a judgement is made under.

use cert_issuer_common::capability::Capability;
use cert_issuer_common::hierarchy::Hierarchy;
use polar_agent_builder::topics::TopicEntry;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The issuer the render names for a workload the credentials agent issues (`infra/rules.nu`).
pub const CREDENTIALS_AGENT: &str = "credentials-agent";

/// The format of the model document this crate reads.
pub const FORMAT: u32 = 1;

/// Every workload of a deployment, whoever issues it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    pub format: u32,
    pub workloads: Vec<Workload>,
}

/// One workload: its name, its one identity (`<workload>.<namespace>.serviceaccount.<domain>`), who issues its
/// certificates, whether it is a bootstrap service, the groups its certificate carries, and its whole set of topics as
/// its scaffold prints them (declared, family and implied), each with what it says about causes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workload {
    pub workload: String,
    pub identity: String,
    pub issuer: String,
    pub bootstrap: bool,
    /// How many of its pods run at once (docs/architecture/authority-model.md, "Multi-instance workloads"). Absent in a
    /// model written before it existed, which reads as one.
    #[serde(default)]
    pub instances: Instances,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub topics: Vec<TopicEntry>,
}

/// How many pods of a workload run at once: one, or many, each an instance of the one identity with its own certificate
/// and session, its instance written into its certificates by issuance (docs/architecture/authority-model.md,
/// "Multi-instance workloads").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Instances {
    #[default]
    One,
    Many,
}

impl Model {
    /// The model in `text`. Refused when it is not the document, not its format, or names a workload twice, or by an
    /// identity that is not its own (the model knows a workload by one identity: authority-model.md, "Decisions").
    pub fn parse(text: &str) -> Result<Self, String> {
        let model: Model = serde_json::from_str(text)
            .map_err(|e| format!("the authority model is not its document: {e}"))?;
        if model.format != FORMAT {
            return Err(format!(
                "the authority model is format {}, and this reads format {FORMAT}",
                model.format
            ));
        }
        let mut names = BTreeSet::new();
        let mut identities = BTreeSet::new();
        for w in &model.workloads {
            if !names.insert(w.workload.as_str()) {
                return Err(format!("the authority model names {} twice", w.workload));
            }
            if !identities.insert(w.identity.as_str()) {
                return Err(format!(
                    "the authority model gives two workloads the identity {}",
                    w.identity
                ));
            }
            if cert_issuer_common::identity::workload_of(&w.identity) != Some(w.workload.as_str()) {
                return Err(format!(
                    "{}: its identity {} names another workload",
                    w.workload, w.identity
                ));
            }
        }
        Ok(model)
    }

    /// How many pods of the workload known by `identity` run at once: one for an identity the model does not know.
    pub fn instances_of(&self, identity: &str) -> Instances {
        self.workloads
            .iter()
            .find(|w| w.identity == identity)
            .map_or(Instances::One, |w| w.instances)
    }

    /// The workload named `name`.
    pub fn workload(&self, name: &str) -> Option<&Workload> {
        self.workloads.iter().find(|w| w.workload == name)
    }

    /// Every topic any workload uses, each once.
    pub(crate) fn topic_names(&self) -> BTreeSet<String> {
        self.workloads
            .iter()
            .flat_map(|w| w.topics.iter().map(|t| t.name.clone()))
            .collect()
    }

    /// Every identity the model knows.
    pub(crate) fn identities(&self) -> BTreeSet<&str> {
        self.workloads.iter().map(|w| w.identity.as_str()).collect()
    }
}

/// What a workload's certificates carry of it: its declared and family topics, as `p:pub:` and `p:sub:` names, and its
/// groups, as `g:` names (docs/architecture/policy-enforcement.md, stage 2). Its implied topics are not written: they
/// follow from its identity and its groups. Each once, sorted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CertificateNames {
    pub publish: Vec<String>,
    pub subscribe: Vec<String>,
    pub groups: Vec<String>,
}

impl CertificateNames {
    /// The names as a certificate carries them.
    pub fn san_strings(&self) -> Vec<String> {
        use cert_issuer_common::group::Group;
        use cert_issuer_common::permission::{TopicAccess, TopicPermission};
        let topic = |access, topic: &String| {
            TopicPermission {
                access,
                topic: topic.clone(),
            }
            .to_san_string()
        };
        self.publish
            .iter()
            .map(|t| topic(TopicAccess::Publish, t))
            .chain(
                self.subscribe
                    .iter()
                    .map(|t| topic(TopicAccess::Subscribe, t)),
            )
            .chain(self.groups.iter().map(|g| Group(g.clone()).to_san_string()))
            .collect()
    }
}

impl Workload {
    /// What this workload's certificates carry.
    pub fn certificate_names(&self) -> CertificateNames {
        let own = |access: polar_agent_builder::Access| -> Vec<String> {
            self.topics
                .iter()
                .filter(|t| {
                    t.access == access
                        && !matches!(
                            t.origin,
                            polar_agent_builder::topics::Origin::Implied { .. }
                        )
                })
                .map(|t| t.name.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        CertificateNames {
            publish: own(polar_agent_builder::Access::Publish),
            subscribe: own(polar_agent_builder::Access::Subscribe),
            groups: self
                .groups
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }
}

impl Model {
    /// What the certificates of the workload known by `identity` carry, when the credentials agent issues it; none for a
    /// workload it does not issue (a bootstrap service is issued by cert-issuer, with cert-issuer's floor) or an identity
    /// the model does not know. Issuance asks this by the identity cert-issuer's validation returns, never by the token's
    /// subject (docs/architecture/authority-model.md, "Decisions": one identity).
    pub fn issued_by_the_credentials_agent(&self, identity: &str) -> Option<CertificateNames> {
        self.workloads
            .iter()
            .find(|w| w.identity == identity && w.issuer == CREDENTIALS_AGENT && !w.bootstrap)
            .map(Workload::certificate_names)
    }
}

/// A person the authority knows (an account in its ledger), for what a change does to them: their identity as Cedar
/// sees it, the groups their certificate carries and the capabilities it holds. A person's identity implies no topics,
/// so only a permit gives them one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub identity: String,
    pub groups: Vec<String>,
    pub capabilities: Vec<Capability>,
}

/// A policy and a groups document, as they stand or as they would: what a judgement is made under. No policy is the
/// floor alone. `memberships` replaces, for Cedar, the groups a certificate carries for each identity it names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pair {
    pub policy: Option<String>,
    pub memberships: BTreeMap<String, Vec<String>>,
    pub hierarchy: Hierarchy,
}
