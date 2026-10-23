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

//! The credentials agent judges a `policy` or `groups` document before it is accepted (docs/architecture/authority-model.md,
//! "Build order", slice 7): the pair as it would stand, against the pair in force, over the deployment's model and every
//! account in its ledger. First it converts the usernames the author wrote to the account IDs they are known by
//! ([`crate::authority_names`]); a name no account has refuses the document. A document under which an invariant that
//! holds would not is refused whole, with its reasons, and recorded in the ledger (`AuthorityRefused`); one that is
//! accepted carries a summary of its effect into the outcome, and leaves its converted body and its whole effect for the
//! publisher, which puts them in force and in the ledger (`AuthorityPublished`). What a reader is told names a person by
//! username.

use crate::authority_names::{Names, convert, legible};
use crate::authority_publisher::body_of;
use crate::authority_sections::{GROUPS, POLICY};
use crate::ledger::LedgerMessage;
use cert_issuer_common::group::extract_groups;
use cert_issuer_common::permission::extract_own_identity;
use credentials_agent_common::authority_update::AuthorityBody;
use credentials_agent_common::events::{AuthorityEffect, CredentialsEvent};
use credentials_agent_model::{Judgement, Model, Pair, Person};
use polar_agent_builder::runtime::{Candidate, RuntimeValue, SectionJudge};
use ractor::ActorRef;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

/// What an outcome says when the deployment gave no model to judge by.
pub const NOT_JUDGED: &str = "not judged: this deployment gave the credentials agent no authority model (CREDENTIALS_AGENT_AUTHORITY_MODEL)";

/// A document the judge accepted: its body as it is put in force (each username its account ID), and what the model
/// judged it to do, when there was a model.
#[derive(Debug, Clone, PartialEq)]
pub struct Acceptance {
    pub body: AuthorityBody,
    pub effect: Option<AuthorityEffect>,
}

/// The documents accepted and not yet published, by section and version: the judge holds each, and the publisher takes
/// it when it publishes that version.
#[derive(Clone, Default)]
pub struct Acceptances(Arc<Mutex<HashMap<(String, u64), Acceptance>>>);

impl Acceptances {
    pub(crate) fn hold(&self, section: &str, version: u64, acceptance: Acceptance) {
        self.0
            .lock()
            .expect("the acceptances are never held across a panic")
            .insert((section.to_string(), version), acceptance);
    }

    /// What was accepted of `section` at `version`, once: the publisher publishes it.
    pub fn take(&self, section: &str, version: u64) -> Option<Acceptance> {
        self.0
            .lock()
            .expect("the acceptances are never held across a panic")
            .remove(&(section.to_string(), version))
    }
}

/// The credentials agent's judge of its `policy` and `groups` sections.
pub struct AuthorityJudge {
    /// The deployment's model; none when the render gave none.
    pub model: Option<Arc<Model>>,
    pub ledger: ActorRef<LedgerMessage>,
    pub acceptances: Acceptances,
}

/// A section's body as it is put in force: read, and each username converted to its account ID.
pub(crate) fn in_force_body(
    section: &str,
    body: &serde_json::Value,
    names: &Names,
) -> Result<AuthorityBody, Vec<String>> {
    convert(body_of(section, body).map_err(|e| vec![e])?, names)
}

/// The pair `policy` and `groups` make, as they are in force; either may be absent (none yet in force).
fn pair_of(policy: Option<AuthorityBody>, groups: Option<AuthorityBody>) -> Pair {
    let mut pair = Pair::default();
    if let Some(AuthorityBody::Policy { source }) = policy {
        pair.policy = Some(source);
    }
    if let Some(AuthorityBody::Groups {
        memberships,
        hierarchy,
    }) = groups
    {
        pair.memberships = memberships;
        pair.hierarchy = hierarchy;
    }
    pair
}

/// Each account's username and account ID, from the ledger.
pub(crate) async fn account_names(ledger: &ActorRef<LedgerMessage>) -> Result<Names, String> {
    ractor::call!(ledger, LedgerMessage::AccountNames).map_err(|e| {
        format!("the ledger did not say which accounts it holds, so no username could be read: {e}")
    })
}

/// Each person the ledger holds an account for, as the broker and Cedar see them: the identity their certificate
/// carries (its first DNS name, their account ID), its groups, and the capabilities the ledger records.
async fn persons(ledger: &ActorRef<LedgerMessage>) -> Vec<Person> {
    let accounts = match ractor::call!(ledger, LedgerMessage::ActiveAccounts) {
        Ok(accounts) => accounts,
        Err(e) => {
            warn!(
                "the ledger did not say which accounts it holds; judging for workloads alone: {e}"
            );
            return Vec::new();
        }
    };
    accounts
        .into_iter()
        .filter_map(|record| {
            let identity = extract_own_identity(&record.certificate_pem).ok()?;
            let groups = extract_groups(&record.certificate_pem)
                .map(|groups| groups.into_iter().map(|g| g.0).collect())
                .unwrap_or_default();
            Some(Person {
                identity,
                groups,
                capabilities: record.capabilities,
            })
        })
        .collect()
}

/// A refusal's reasons, in the fixed form (docs/architecture/authority-model.md, "Decisions"): for each goal lost, the
/// invariant, what breaks it, the request now denied and the rules that deny it, and then what used to hold it.
fn refusal_reasons(judgement: &Judgement) -> Vec<String> {
    judgement
        .losses
        .iter()
        .flat_map(|loss| {
            [
                loss.to_string(),
                format!("it held through: {}", loss.path()),
            ]
        })
        .collect()
}

/// What a judgement says a change does, as the ledger keeps it.
fn effect_of(judgement: &Judgement) -> AuthorityEffect {
    let request =
        |(identity, topic, action): &(String, String, credentials_agent_model::Action)| {
            format!("{identity} {} {topic}", action.as_str())
        };
    let round_trip =
        |(workload, entry, sent): &(String, polar_agent_builder::topics::CauseEntry, String)| {
            format!("{workload} {entry:?} {sent}")
        };
    AuthorityEffect {
        gained: judgement.effect.gained.iter().map(request).collect(),
        lost: judgement.effect.lost.iter().map(request).collect(),
        began_closing: judgement
            .effect
            .began_closing
            .iter()
            .map(round_trip)
            .collect(),
        stopped_closing: judgement
            .effect
            .stopped_closing
            .iter()
            .map(round_trip)
            .collect(),
        unknown_keys: judgement.unknown_keys.clone(),
    }
}

/// An effect as a reader is told it: each person by username.
fn legible_effect(effect: AuthorityEffect, names: &Names) -> AuthorityEffect {
    let all = |items: Vec<String>| items.iter().map(|item| legible(item, names)).collect();
    AuthorityEffect {
        gained: all(effect.gained),
        lost: all(effect.lost),
        began_closing: all(effect.began_closing),
        stopped_closing: all(effect.stopped_closing),
        unknown_keys: all(effect.unknown_keys),
    }
}

/// The summary an accepted document's outcome carries.
fn notes_of(judgement: &Judgement) -> Vec<String> {
    let effect = &judgement.effect;
    let losing: std::collections::BTreeSet<&str> =
        effect.lost.iter().map(|(i, _, _)| i.as_str()).collect();
    let gaining: std::collections::BTreeSet<&str> =
        effect.gained.iter().map(|(i, _, _)| i.as_str()).collect();
    let mut notes = vec![format!(
        "judged by the authority model: {} identities lose {} uses of a topic, {} gain {}; {} round trips stop closing, {} begin",
        losing.len(),
        effect.lost.len(),
        gaining.len(),
        effect.gained.len(),
        effect.stopped_closing.len(),
        effect.began_closing.len()
    )];
    if !judgement.unknown_keys.is_empty() {
        notes.push(format!(
            "the groups document names no identity the authority knows: {}",
            judgement.unknown_keys.join(", ")
        ));
    }
    notes
}

impl AuthorityJudge {
    /// Records the refusal of `candidate` in the ledger, and gives back its reasons.
    async fn refuse(&self, candidate: &Candidate<'_>, reasons: Vec<String>) -> Vec<String> {
        warn!(
            section = candidate.section,
            version = candidate.version,
            ?reasons,
            "refused by the credentials agent"
        );
        let refused = CredentialsEvent::AuthorityRefused {
            section: candidate.section.to_string(),
            version: candidate.version,
            signer: candidate.signer.to_string(),
            reasons: reasons.clone(),
            timestamp: polar::time::now_millis(),
        };
        match ractor::call!(self.ledger, |reply| LedgerMessage::Append {
            event: refused,
            reply
        }) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!("the refusal was not recorded in the ledger: {e}"),
            Err(e) => warn!("the ledger did not answer to record the refusal: {e}"),
        }
        reasons
    }
}

#[ractor::async_trait]
impl SectionJudge for AuthorityJudge {
    async fn judge(
        &self,
        candidate: &Candidate<'_>,
        in_force: &HashMap<String, RuntimeValue>,
    ) -> Result<Vec<String>, Vec<String>> {
        if candidate.section != POLICY.name && candidate.section != GROUPS.name {
            return Ok(Vec::new());
        }
        let names = match account_names(&self.ledger).await {
            Ok(names) => names,
            Err(e) => return Err(self.refuse(candidate, vec![e]).await),
        };
        let proposed_body = match in_force_body(candidate.section, candidate.body, &names) {
            Ok(body) => body,
            Err(reasons) => return Err(self.refuse(candidate, reasons).await),
        };
        let Some(model) = self.model.clone() else {
            self.acceptances.hold(
                candidate.section,
                candidate.version,
                Acceptance {
                    body: proposed_body,
                    effect: None,
                },
            );
            return Ok(vec![NOT_JUDGED.to_string()]);
        };
        // The sections in force, as they were put in force.
        let held = |section: &str| -> Result<Option<AuthorityBody>, Vec<String>> {
            in_force
                .get(section)
                .map(|value| in_force_body(section, &value.body, &names))
                .transpose()
        };
        let (policy, groups) = match (held(POLICY.name), held(GROUPS.name)) {
            (Ok(policy), Ok(groups)) => (policy, groups),
            (Err(reasons), _) | (_, Err(reasons)) => {
                return Err(self.refuse(candidate, reasons).await);
            }
        };
        let current = pair_of(policy.clone(), groups.clone());
        let proposed = if candidate.section == POLICY.name {
            pair_of(Some(proposed_body.clone()), groups)
        } else {
            pair_of(policy, Some(proposed_body.clone()))
        };
        let persons = persons(&self.ledger).await;
        // The work of a judgement is the model's; off the actor's thread.
        let judged = tokio::task::spawn_blocking(move || {
            credentials_agent_model::judge(&model, &current, &proposed, &persons)
        })
        .await
        .map_err(|e| format!("the authority model could not judge it: {e}"))
        .and_then(|judged| judged);
        let mut judgement = match judged {
            Ok(judgement) => judgement,
            Err(e) => return Err(self.refuse(candidate, vec![e]).await),
        };
        // A person provisioned and yet to claim has no certificate for the model to know them by, and the ledger knows
        // them: their account ID is no unknown name.
        judgement
            .unknown_keys
            .retain(|key| !names.values().any(|account_id| account_id == key));
        if judgement.refuses() {
            let reasons = refusal_reasons(&judgement)
                .iter()
                .map(|r| legible(r, &names))
                .collect();
            return Err(self.refuse(candidate, reasons).await);
        }
        info!(
            section = candidate.section,
            version = candidate.version,
            "accepted by the authority model"
        );
        self.acceptances.hold(
            candidate.section,
            candidate.version,
            Acceptance {
                body: proposed_body,
                effect: Some(legible_effect(effect_of(&judgement), &names)),
            },
        );
        Ok(notes_of(&judgement)
            .iter()
            .map(|n| legible(n, &names))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{LedgerActor, LedgerActorArgs};
    use ractor::Actor;
    use serde_json::json;

    const FIXTURE: &str = include_str!("../../model/tests/fixtures/sandbox-model.json");

    fn id(workload: &str) -> String {
        format!("{workload}.polar.serviceaccount.cluster.local")
    }

    async fn ledger() -> (ActorRef<LedgerMessage>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let (ledger, _) = Actor::spawn(
            None,
            LedgerActor,
            LedgerActorArgs {
                log_path: dir
                    .path()
                    .join("events.jsonl")
                    .to_str()
                    .unwrap()
                    .to_string(),
                projection_actor: crate::projection::tests::fake_projection_actor().await,
            },
        )
        .await
        .unwrap();
        (ledger, dir)
    }

    fn judge_with(ledger: &ActorRef<LedgerMessage>, model: bool) -> (AuthorityJudge, Acceptances) {
        let acceptances = Acceptances::default();
        (
            AuthorityJudge {
                model: model.then(|| Arc::new(Model::parse(FIXTURE).unwrap())),
                ledger: ledger.clone(),
                acceptances: acceptances.clone(),
            },
            acceptances,
        )
    }

    fn policy(source: &str) -> serde_json::Value {
        json!({ "source": source })
    }

    fn held(body: serde_json::Value) -> RuntimeValue {
        RuntimeValue {
            version: 1,
            body,
            signer: "alice (serial 01)".to_string(),
        }
    }

    async fn judged(
        judge: &AuthorityJudge,
        section: &str,
        version: u64,
        body: &serde_json::Value,
        in_force: &HashMap<String, RuntimeValue>,
    ) -> Result<Vec<String>, Vec<String>> {
        judge
            .judge(
                &Candidate {
                    section,
                    version,
                    signer: "alice (serial 01)",
                    body,
                },
                in_force,
            )
            .await
    }

    async fn refusals(ledger: &ActorRef<LedgerMessage>) -> Vec<CredentialsEvent> {
        let events = ractor::call!(ledger, |reply| LedgerMessage::GetFullHistory { reply })
            .unwrap()
            .unwrap();
        events
            .into_iter()
            .map(|(_, e)| e)
            .filter(|e| matches!(e, CredentialsEvent::AuthorityRefused { .. }))
            .collect()
    }

    /// Open point B's own test (docs/architecture/policy-enforcement.md): a policy forbidding a workload
    /// `polar-authority-updates` is refused whole, and it is recorded.
    #[tokio::test]
    async fn a_policy_forbidding_a_workload_the_authority_updates_is_refused_whole_and_recorded() {
        let (ledger, _dir) = ledger().await;
        let (judge, acceptances) = judge_with(&ledger, true);
        let forbid = policy(&format!(
            r#"forbid (principal == Identity::"{}", action == Action::"subscribe", resource == Topic::"polar-authority-updates");"#,
            id("kube-observer")
        ));
        let reasons = judged(&judge, "policy", 2, &forbid, &HashMap::new())
            .await
            .unwrap_err();
        assert!(
            reasons[0].starts_with("hearing authority would stop"),
            "{reasons:?}"
        );
        assert!(reasons[0].contains("kube-observer"), "{reasons:?}");
        assert!(reasons[0].contains("denied by policy0"), "{reasons:?}");
        assert!(
            reasons[1].starts_with("it held through: credentials-agent publishes"),
            "{reasons:?}"
        );
        match refusals(&ledger).await.as_slice() {
            [
                CredentialsEvent::AuthorityRefused {
                    section,
                    version,
                    signer,
                    reasons: recorded,
                    ..
                },
            ] => {
                assert_eq!(
                    (section.as_str(), *version, signer.as_str()),
                    ("policy", 2, "alice (serial 01)")
                );
                assert_eq!(recorded, &reasons);
            }
            other => panic!("not recorded once: {other:?}"),
        }
        // A refused document leaves no effect to publish.
        assert!(acceptances.take("policy", 2).is_none());
    }

    #[tokio::test]
    async fn a_policy_cutting_renewal_is_refused_as_breaking_correction() {
        let (ledger, _dir) = ledger().await;
        let (judge, _) = judge_with(&ledger, true);
        let forbid = policy(&format!(
            r#"forbid (principal == Identity::"{}", action == Action::"publish", resource == Topic::"credentials-workload-issuance-requests");"#,
            id("credentials-agent-observer")
        ));
        let reasons = judged(&judge, "policy", 2, &forbid, &HashMap::new())
            .await
            .unwrap_err();
        assert!(
            reasons
                .iter()
                .any(|r| r.starts_with("correction would stop") && r.contains("renewal")),
            "{reasons:?}"
        );
    }

    #[tokio::test]
    async fn a_groups_document_is_judged_against_the_policy_in_force() {
        let (ledger, _dir) = ledger().await;
        let (judge, _) = judge_with(&ledger, true);
        let in_force = HashMap::from([(
            "policy".to_string(),
            held(policy(
                r#"forbid (principal in Group::"retired", action == Action::"publish", resource);"#,
            )),
        )]);
        let moved = json!({ "memberships": { id("polar-scheduler"): ["retired"] } });
        let reasons = judged(&judge, "groups", 1, &moved, &in_force)
            .await
            .unwrap_err();
        assert!(
            reasons
                .iter()
                .any(|r| r.starts_with("correction would stop")),
            "{reasons:?}"
        );
        // The same groups document with no policy in force takes nothing.
        assert!(
            judged(&judge, "groups", 1, &moved, &HashMap::new())
                .await
                .is_ok()
        );
    }

    /// A groups document keys a person by username, converted to their account ID; a workload identity the catalog
    /// does not know is reported and the document accepted (it may be deliberate, a workload not yet deployed); a
    /// username no account has refuses it.
    #[tokio::test]
    async fn a_groups_document_s_usernames_are_converted_and_a_name_that_applies_to_no_one_is_refused()
     {
        let (ledger, _dir) = ledger().await;
        let (judge, acceptances) = judge_with(&ledger, true);
        let alice = "d983a3d2-9b61-41c5-88a0-d123fb50ba33";
        crate::ledger::tests::holds_account(&ledger, "alice", alice).await;
        let typo = "polar-schedular.polar.serviceaccount.cluster.local";
        let doc = json!({ "memberships": { "alice": ["operators"], typo: [], id("polar-scheduler"): [] } });
        let notes = judged(&judge, "groups", 1, &doc, &HashMap::new())
            .await
            .unwrap();
        assert!(
            notes
                .iter()
                .any(|n| n.contains("names no identity the authority knows") && n.contains(typo)),
            "{notes:?}"
        );
        assert!(
            !notes
                .iter()
                .any(|n| n.contains("alice") || n.contains(alice)),
            "{notes:?}"
        );
        let accepted = acceptances.take("groups", 1).expect("accepted");
        assert_eq!(accepted.effect.expect("judged").unknown_keys, [typo]);
        match accepted.body {
            AuthorityBody::Groups { memberships, .. } => {
                assert_eq!(memberships[alice], ["operators"]);
                assert!(!memberships.contains_key("alice"));
            }
            other => panic!("not groups: {other:?}"),
        }
        let stranger = json!({ "memberships": { "carol": ["operators"] } });
        let reasons = judged(&judge, "groups", 2, &stranger, &HashMap::new())
            .await
            .unwrap_err();
        assert!(
            reasons[0].starts_with("`carol` is no account's username"),
            "{reasons:?}"
        );
        assert_eq!(refusals(&ledger).await.len(), 1);
        assert!(acceptances.take("groups", 2).is_none());
    }

    /// The sections in force are judged as they were put in force: a groups document in force that names alice by
    /// username puts her account in the group a new policy then grants.
    #[tokio::test]
    async fn the_sections_in_force_are_judged_with_their_usernames_converted() {
        let (ledger, _dir) = ledger().await;
        let (judge, acceptances) = judge_with(&ledger, true);
        crate::ledger::tests::holds_account(
            &ledger,
            "alice",
            "d983a3d2-9b61-41c5-88a0-d123fb50ba33",
        )
        .await;
        let in_force = HashMap::from([(
            "groups".to_string(),
            held(json!({ "memberships": { "alice": ["readers"] } })),
        )]);
        let permit = policy(
            r#"permit (principal in Group::"readers", action == Action::"subscribe", resource == Topic::"polar-authority-updates");"#,
        );
        judged(&judge, "policy", 2, &permit, &in_force)
            .await
            .unwrap();
        assert_eq!(
            acceptances
                .take("policy", 2)
                .expect("accepted")
                .effect
                .expect("judged")
                .gained,
            ["alice subscribe polar-authority-updates"]
        );
    }

    /// A person provisioned and yet to claim their certificate is known to the ledger: an admin may put them in a group
    /// first, and the document says nothing of an unknown name.
    #[tokio::test]
    async fn a_person_provisioned_and_not_yet_claimed_is_no_unknown_key() {
        let (ledger, _dir) = ledger().await;
        let (judge, acceptances) = judge_with(&ledger, true);
        let fay = "0c9e7d4a-1b2f-4e6a-8c3d-5f7a9b1e2d40";
        ractor::call!(ledger, |reply| LedgerMessage::Append {
            event: crate::state::tests::provisioned("fay", fay),
            reply
        })
        .unwrap()
        .unwrap();
        let doc = json!({ "memberships": { "fay": ["operators"] } });
        let notes = judged(&judge, "groups", 1, &doc, &HashMap::new())
            .await
            .unwrap();
        assert!(
            !notes.iter().any(|n| n.contains("names no identity")),
            "{notes:?}"
        );
        let accepted = acceptances.take("groups", 1).expect("accepted");
        assert!(accepted.effect.expect("judged").unknown_keys.is_empty());
    }

    #[tokio::test]
    async fn an_accepted_document_carries_a_summary_and_leaves_its_effect_for_the_publisher() {
        let (ledger, _dir) = ledger().await;
        let (judge, acceptances) = judge_with(&ledger, true);
        // An account the ledger holds is judged for too, written by username and known by account ID.
        let account = "7f3c2d0e-0000-4000-8000-000000000001";
        crate::ledger::tests::holds_account(&ledger, "alice", account).await;
        let permit = policy(
            r#"permit (principal == Identity::"alice", action == Action::"subscribe", resource == Topic::"polar-authority-updates");"#,
        );
        let notes = judged(&judge, "policy", 3, &permit, &HashMap::new())
            .await
            .unwrap();
        assert!(
            notes[0].starts_with(
                "judged by the authority model: 0 identities lose 0 uses of a topic, 1 gain 1"
            ),
            "{notes:?}"
        );
        let accepted = acceptances
            .take("policy", 3)
            .expect("held for the publisher");
        // The reader is told the username; what is put in force names the account ID.
        assert_eq!(
            accepted.effect.expect("judged").gained,
            ["alice subscribe polar-authority-updates"]
        );
        match accepted.body {
            AuthorityBody::Policy { source } => assert_eq!(
                polar_policy::identities_named(&source).unwrap(),
                [account.to_string()].into()
            ),
            other => panic!("not a policy: {other:?}"),
        }
        assert!(acceptances.take("policy", 3).is_none(), "taken once");
        assert!(refusals(&ledger).await.is_empty());
    }

    #[tokio::test]
    async fn with_no_model_a_document_is_accepted_and_said_not_judged() {
        let (ledger, _dir) = ledger().await;
        let (judge, acceptances) = judge_with(&ledger, false);
        let notes = judged(
            &judge,
            "policy",
            1,
            &policy("forbid (principal, action, resource);"),
            &HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(notes, [NOT_JUDGED]);
        // Not judged, and still converted: what is published is held, with no effect.
        assert_eq!(
            acceptances.take("policy", 1),
            Some(Acceptance {
                body: AuthorityBody::Policy {
                    source: "forbid (principal, action, resource);".to_string()
                },
                effect: None
            })
        );
        // A username no account has is refused with no model too: converting is not judging.
        let unknown = policy(r#"permit (principal == Identity::"carol", action, resource);"#);
        assert!(
            judged(&judge, "policy", 2, &unknown, &HashMap::new())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_section_that_is_not_authority_is_not_judged() {
        let (ledger, _dir) = ledger().await;
        let (judge, _) = judge_with(&ledger, true);
        assert_eq!(
            judged(&judge, "other", 1, &json!({}), &HashMap::new()).await,
            Ok(vec![])
        );
    }

    /// The whole path, slice 6's configuration actor with this judge: an ordinary policy, naming a person by username,
    /// is applied with the judge's summary and put in force by account ID; a signed policy that would stop a workload
    /// hearing authority is refused as `invalid` with the judge's reasons, the policy in force stays, and the refusal
    /// is recorded.
    #[tokio::test]
    async fn through_the_configuration_actor_a_refused_policy_leaves_the_one_in_force() {
        use polar_agent_builder::configuration::{ApplyOutcome, ConfigurationHandle, Refusal};
        use polar_agent_builder::runtime::RuntimeSection;
        use polar_agent_builder::signed::UpdateVerifier;
        use polar_signing::testing::{Spec, now, root, signer};

        struct Parent;
        #[ractor::async_trait]
        impl Actor for Parent {
            type Msg = ();
            type State = ();
            type Arguments = ();
            async fn pre_start(
                &self,
                _: ActorRef<()>,
                _: (),
            ) -> Result<(), ractor::ActorProcessingErr> {
                Ok(())
            }
        }
        static SECTIONS: [&RuntimeSection; 2] = [&POLICY, &GROUPS];
        const AGENT: &str = "credentials-agent";

        let (ledger, _dir) = ledger().await;
        let (judge, acceptances) = judge_with(&ledger, true);
        let ca = root("Polar Internal CA");
        let verifier = UpdateVerifier::new(
            polar_signing::Trust::from_pem(std::slice::from_ref(&ca.pem)).unwrap(),
            AGENT,
        )
        .with_clock(now);
        let (parent, _) = Actor::spawn(None, Parent, ()).await.unwrap();
        let env: HashMap<String, String> = HashMap::new();
        let config = ConfigurationHandle::spawn_full(
            None,
            AGENT,
            polar_env::Config::builder(&env).build(),
            Some(verifier),
            &SECTIONS,
            None,
            Some(Arc::new(judge)),
            parent.get_cell(),
        )
        .await
        .unwrap();
        let signed = |version: u64, source: &str| {
            let s = signer(
                &ca,
                Spec {
                    capabilities: vec!["c:update-policy"],
                    ..Spec::default()
                },
            );
            polar_signing::sign(
                json!({"type": "runtime", "id": format!("policy-{version}"), "target": {"agent": AGENT},
                       "section": "policy", "version": version, "body": {"source": source}}),
                &[s.cert_pem],
                &s.key_pem,
            )
            .unwrap()
            .to_value()
        };

        crate::ledger::tests::holds_account(
            &ledger,
            "alice",
            polar_signing::testing::ALICE_ACCOUNT_ID,
        )
        .await;
        let ordinary = format!(
            r#"permit (principal == Identity::"{}", action == Action::"subscribe", resource == Topic::"credentials-workload-issuance-replies");
permit (principal == Identity::"alice", action == Action::"subscribe", resource == Topic::"credentials-workload-issuance-replies");"#,
            id("harness-sink")
        );
        match config.submit(signed(1, &ordinary)).await.unwrap() {
            ApplyOutcome::Applied { changed, notes } => {
                assert_eq!(changed, ["policy"]);
                assert!(
                    notes[0].starts_with("judged by the authority model"),
                    "{notes:?}"
                );
            }
            other => panic!("not applied: {other:?}"),
        }
        // Put in force by account ID; the document in force stays as it was signed.
        match acceptances.take("policy", 1).expect("accepted").body {
            AuthorityBody::Policy { source } => {
                assert!(
                    source.contains(polar_signing::testing::ALICE_ACCOUNT_ID),
                    "{source}"
                )
            }
            other => panic!("not a policy: {other:?}"),
        }

        let cutting = format!(
            r#"forbid (principal == Identity::"{}", action == Action::"subscribe", resource == Topic::"polar-authority-updates");"#,
            id("kube-observer")
        );
        match config.submit(signed(2, &cutting)).await.unwrap() {
            ApplyOutcome::Refused(Refusal::Invalid(reasons)) => {
                assert!(
                    reasons[0].starts_with("hearing authority would stop"),
                    "{reasons:?}"
                )
            }
            other => panic!("not refused as invalid: {other:?}"),
        }
        let held = config
            .runtime("policy")
            .await
            .unwrap()
            .expect("a policy in force");
        assert_eq!((held.version, held.body), (1, json!({"source": ordinary})));
        match refusals(&ledger).await.as_slice() {
            [
                CredentialsEvent::AuthorityRefused {
                    version: 2, signer, ..
                },
            ] => {
                assert!(signer.starts_with("alice"), "{signer}")
            }
            other => panic!("not recorded once: {other:?}"),
        }
    }

    /// Whatever document it is given, the judge refuses exactly when the model does, with two reasons for each goal
    /// lost, and records a refusal in the ledger only when it refuses, and only that once.
    mod props {
        use super::*;
        use proptest::prelude::*;

        /// A permit or forbid of one of the model's own requests: a workload hearing authority, or its use of one of
        /// its own topics. Forbids are three in four, and half of them are of hearing authority, so that most runs
        /// hold refusals as well as acceptances.
        fn document() -> impl Strategy<Value = (bool, usize, usize)> {
            (prop::bool::weighted(0.75), 0..64usize, 0..64usize)
        }

        fn source(model: &Model, (forbid, w, t): (bool, usize, usize)) -> String {
            let workload = &model.workloads[w % model.workloads.len()];
            let (topic, access) = match workload.topics.get(t / 2 % workload.topics.len().max(1)) {
                Some(entry) if t % 2 == 1 => (entry.name.as_str(), entry.access),
                _ => (
                    credentials_agent_common::authority_update::AUTHORITY_UPDATES_TOPIC,
                    polar_agent_builder::Access::Subscribe,
                ),
            };
            format!(
                r#"{} (principal == Identity::"{}", action == Action::"{}", resource == Topic::"{topic}");"#,
                if forbid { "forbid" } else { "permit" },
                workload.identity,
                access.as_str()
            )
        }

        proptest! {
            #![proptest_config(ProptestConfig { cases: 16, ..ProptestConfig::default() })]
            #[test]
            fn the_judge_refuses_exactly_when_the_model_does_and_records_only_its_refusals(doc in document()) {
                let runtime = tokio::runtime::Runtime::new().unwrap();
                runtime.block_on(async {
                    let (ledger, _dir) = ledger().await;
                    let (judge, acceptances) = judge_with(&ledger, true);
                    let model = judge.model.clone().unwrap();
                    let text = source(&model, doc);
                    let expected = credentials_agent_model::judge(
                        &model,
                        &Pair::default(),
                        &Pair {
                            policy: Some(text.clone()),
                            ..Pair::default()
                        },
                        &[],
                    )
                    .unwrap();
                    let outcome = judged(&judge, "policy", 2, &policy(&text), &HashMap::new()).await;
                    let recorded = refusals(&ledger).await;
                    match outcome {
                        Err(reasons) => {
                            prop_assert!(expected.refuses(), "{text}");
                            prop_assert_eq!(reasons.len(), 2 * expected.losses.len());
                            prop_assert_eq!(recorded.len(), 1);
                            prop_assert!(acceptances.take("policy", 2).is_none());
                        }
                        Ok(_) => {
                            prop_assert!(!expected.refuses(), "{text}");
                            prop_assert!(recorded.is_empty());
                            prop_assert_eq!(
                                acceptances.take("policy", 2).and_then(|accepted| accepted.effect),
                                Some(effect_of(&expected))
                            );
                        }
                    }
                    Ok(())
                })?;
            }
        }
    }
}
