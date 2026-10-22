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

//! A person is written by username and known by account ID (docs/architecture/authority-model.md, "Decisions"): the
//! author of a policy or a groups document names a person `alice`, and the credentials agent converts each username to
//! the account ID their certificate carries, from its ledger, before it judges the document and publishes it. A name
//! that is a workload's identity is left as it is; any other is a username, and one no account has refuses the
//! document. What is said to a reader goes the other way ([`legible`]): an account ID is shown as its username.

use cert_issuer_common::identity::workload_of;
use credentials_agent_common::authority_update::AuthorityBody;
use std::collections::BTreeMap;

/// Each account's username and the account ID it is known by (`LedgerMessage::AccountNames`).
pub type Names = BTreeMap<String, String>;

/// `body` as it is put in force: every person it names by username named by their account ID instead. Refused, with
/// one reason for each, when it names a username no account has. A revocation names no one, and is as it was.
pub fn convert(body: AuthorityBody, names: &Names) -> Result<AuthorityBody, Vec<String>> {
    match body {
        AuthorityBody::Policy { source } => {
            let named = polar_policy::identities_named(&source).map_err(|e| vec![e])?;
            let renames = renames(named.iter().map(String::as_str), names)?;
            let source = polar_policy::rename_identities(&source, &renames).map_err(|e| vec![e])?;
            Ok(AuthorityBody::Policy { source })
        }
        AuthorityBody::Groups {
            memberships,
            hierarchy,
        } => {
            let renames = renames(memberships.keys().map(String::as_str), names)?;
            let memberships = memberships
                .into_iter()
                .map(|(key, groups)| (renames.get(&key).cloned().unwrap_or(key), groups))
                .collect();
            Ok(AuthorityBody::Groups {
                memberships,
                hierarchy,
            })
        }
        revoked @ AuthorityBody::Revoked { .. } => Ok(revoked),
    }
}

/// The account ID of each username among `named`; a workload's identity is not renamed, and a name that is neither is
/// a reason to refuse.
fn renames<'a>(
    named: impl Iterator<Item = &'a str>,
    names: &Names,
) -> Result<BTreeMap<String, String>, Vec<String>> {
    let mut renames = BTreeMap::new();
    let mut unknown = Vec::new();
    for name in named {
        if workload_of(name).is_some() {
            continue;
        }
        match names.get(name) {
            Some(account_id) => {
                renames.insert(name.to_string(), account_id.clone());
            }
            None => unknown.push(format!(
                "`{name}` is no account's username, and not a workload's identity \
                 (<workload>.<namespace>.serviceaccount.<domain>), so it would apply to no one"
            )),
        }
    }
    if unknown.is_empty() {
        Ok(renames)
    } else {
        Err(unknown)
    }
}

/// `text` with each account ID in it shown as its username, for a reader.
pub fn legible(text: &str, names: &Names) -> String {
    names
        .iter()
        .fold(text.to_string(), |text, (username, account_id)| {
            text.replace(account_id.as_str(), username)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE: &str = "d983a3d2-9b61-41c5-88a0-d123fb50ba33";
    const BOB: &str = "5b1f0c2e-7a44-4c1d-9e3b-2f6a8d0c4e11";
    const OBSERVER: &str = "kube-observer.polar.serviceaccount.cluster.local";

    fn names() -> Names {
        Names::from([
            ("alice".to_string(), ALICE.to_string()),
            ("bob".to_string(), BOB.to_string()),
        ])
    }

    fn policy(source: &str) -> AuthorityBody {
        AuthorityBody::Policy {
            source: source.to_string(),
        }
    }

    fn source_of(body: AuthorityBody) -> String {
        match body {
            AuthorityBody::Policy { source } => source,
            other => panic!("not a policy: {other:?}"),
        }
    }

    #[test]
    fn a_policy_s_usernames_become_account_ids_and_workload_identities_stay() {
        let written = format!(
            r#"permit (principal == Identity::"alice", action == Action::"subscribe", resource == Topic::"news");
forbid (principal, action, resource) when {{ principal == Identity::"bob" }};
permit (principal == Identity::"{OBSERVER}", action, resource);"#
        );
        let converted = source_of(convert(policy(&written), &names()).unwrap());
        assert_eq!(
            polar_policy::identities_named(&converted).unwrap(),
            [ALICE, BOB, OBSERVER].map(str::to_string).into()
        );
        // A policy naming no person is as it was written.
        let workloads_only =
            format!(r#"permit (principal == Identity::"{OBSERVER}", action, resource);"#);
        assert_eq!(
            source_of(convert(policy(&workloads_only), &names()).unwrap()),
            workloads_only
        );
    }

    #[test]
    fn a_name_no_account_has_refuses_the_document_naming_each() {
        let written = r#"permit (principal == Identity::"alise", action, resource);
permit (principal == Identity::"kube-observer", action, resource);
permit (principal == Identity::"d983a3d2-9b61-41c5-88a0-d123fb50ba33", action, resource);"#;
        let reasons = convert(policy(written), &names()).unwrap_err();
        assert_eq!(reasons.len(), 3, "{reasons:?}");
        for name in ["alise", "kube-observer", ALICE] {
            assert!(
                reasons
                    .iter()
                    .any(|r| r.contains(&format!("`{name}`")) && r.contains("no account")),
                "{name}: {reasons:?}"
            );
        }
        let groups = AuthorityBody::Groups {
            memberships: BTreeMap::from([("carol".to_string(), vec!["operators".to_string()])]),
            hierarchy: Default::default(),
        };
        let reasons = convert(groups, &names()).unwrap_err();
        assert!(reasons[0].contains("`carol`"), "{reasons:?}");
    }

    #[test]
    fn a_groups_document_s_keys_are_converted_and_its_arrangement_kept() {
        let hierarchy: cert_issuer_common::hierarchy::Hierarchy = serde_json::from_value(
            serde_json::json!({"groups": {"night-shift": {"groups": ["operators"]}}}),
        )
        .unwrap();
        let written = AuthorityBody::Groups {
            memberships: BTreeMap::from([
                ("alice".to_string(), vec!["operators".to_string()]),
                (OBSERVER.to_string(), vec!["night-shift".to_string()]),
            ]),
            hierarchy: hierarchy.clone(),
        };
        assert_eq!(
            convert(written, &names()).unwrap(),
            AuthorityBody::Groups {
                memberships: BTreeMap::from([
                    (ALICE.to_string(), vec!["operators".to_string()]),
                    (OBSERVER.to_string(), vec!["night-shift".to_string()]),
                ]),
                hierarchy,
            }
        );
        let revoked = AuthorityBody::Revoked {
            serials: vec!["01".to_string()],
        };
        assert_eq!(convert(revoked.clone(), &names()).unwrap(), revoked);
    }

    #[test]
    fn what_is_said_to_a_reader_shows_a_username_for_an_account_id() {
        assert_eq!(
            legible(
                &format!("{ALICE} may no longer subscribe on `news`; {BOB} and {OBSERVER} may"),
                &names()
            ),
            format!("alice may no longer subscribe on `news`; bob and {OBSERVER} may")
        );
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        /// Names an author might write: two accounts' usernames, a username no account has, a workload's identity, a
        /// bare workload name, and an account ID written by hand.
        const WRITTEN: &[&str] = &["alice", "bob", "carol", OBSERVER, "kube-observer", ALICE];

        fn expected(name: &str) -> Option<Option<&'static str>> {
            match name {
                "alice" => Some(Some(ALICE)),
                "bob" => Some(Some(BOB)),
                OBSERVER => Some(None),
                _ => None,
            }
        }

        proptest! {
            /// Whatever names a document is written with, it is refused exactly when one of them is neither a
            /// workload's identity nor an account's username, with a reason naming each such; and otherwise each
            /// username is its account ID and each workload identity is as written, in a policy and a groups document
            /// alike.
            #[test]
            fn a_document_is_refused_exactly_for_the_names_that_apply_to_no_one(
                written in proptest::sample::subsequence(WRITTEN.to_vec(), 0..=WRITTEN.len()),
            ) {
                let unknown: Vec<&str> = written.iter().copied().filter(|n| expected(n).is_none()).collect();
                let source = written
                    .iter()
                    .map(|n| format!(r#"permit (principal == Identity::"{n}", action, resource);"#))
                    .collect::<Vec<_>>()
                    .join("\n");
                let groups = AuthorityBody::Groups {
                    memberships: written.iter().map(|n| (n.to_string(), vec!["g".to_string()])).collect(),
                    hierarchy: Default::default(),
                };
                for (body, read) in [
                    (policy(&source), (|b: AuthorityBody| polar_policy::identities_named(&source_of(b)).unwrap()) as fn(AuthorityBody) -> std::collections::BTreeSet<String>),
                    (groups, |b: AuthorityBody| match b {
                        AuthorityBody::Groups { memberships, .. } => memberships.into_keys().collect(),
                        other => panic!("{other:?}"),
                    }),
                ] {
                    match convert(body, &names()) {
                        Err(reasons) => {
                            prop_assert!(!unknown.is_empty());
                            prop_assert_eq!(reasons.len(), unknown.len());
                            for n in &unknown {
                                let quoted = format!("`{n}`");
                                prop_assert!(reasons.iter().any(|r| r.starts_with(&quoted)), "{}: {:?}", n, reasons);
                            }
                        }
                        Ok(converted) => {
                            prop_assert!(unknown.is_empty());
                            let in_force: std::collections::BTreeSet<String> = written
                                .iter()
                                .map(|n| expected(n).flatten().unwrap_or(n).to_string())
                                .collect();
                            prop_assert_eq!(read(converted), in_force);
                        }
                    }
                }
            }
        }
    }
}
