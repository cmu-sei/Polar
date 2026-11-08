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

//! The instance a multi-instance workload's certificate is for (docs/architecture/authority-model.md, "Multi-instance
//! workloads"): its pod, carried as two names of its own beside the identity's DNS name and the `p:`, `g:` and `c:`
//! names, each a URI name as those are:
//!
//! - `i:uid:<pod uid>`, the key: unique, never reused, what the token's `instance_binding` claim holds;
//! - `i:pod:<pod name>`, for people: it names the Job, and so the build or the run.
//!
//! Both come from the pod's bound ServiceAccount token, which cert-issuer verifies; nothing about the instance is the
//! pod's own word. A certificate of a single-instance workload carries neither, and a certificate whose `i:` names do not
//! read as one instance is refused wherever it is read. The supersession rule both the broker and the credentials agent
//! keep is here too ([`same_instance`]), so they cannot disagree.

use crate::permission::PermissionExtractionError;
use serde::{Deserialize, Serialize};

/// What every instance name begins with.
pub const INSTANCE_PREFIX: &str = "i:";
/// The pod UID's name: `i:uid:<uid>`.
pub const UID_PREFIX: &str = "i:uid:";
/// The pod name's name: `i:pod:<name>`.
pub const POD_PREFIX: &str = "i:pod:";

/// The pod a certificate is for: its UID, and its name when the token said it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Instance {
    pub uid: String,
    pub pod: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstanceError {
    #[error(
        "{0:?} is not a pod UID: lower-case letters, digits and '-', at most 64, starting and ending with one of the first two"
    )]
    InvalidUid(String),
    #[error(
        "{0:?} is not a pod name: a DNS subdomain, lower-case letters, digits, '-' and '.', at most 253"
    )]
    InvalidPod(String),
    #[error("{0:?} is not an instance name: `i:uid:<pod uid>` or `i:pod:<pod name>`")]
    UnknownName(String),
    #[error("the certificate names more than one pod UID")]
    TwoUids,
    #[error("the certificate names more than one pod name")]
    TwoPods,
    #[error("the certificate names a pod with no pod UID")]
    PodWithoutUid,
    /// A multi-instance workload's certificate names its pod by name beside its UID, and the token for this pod named
    /// none (docs/architecture/authority-model.md, "Multi-instance workloads": a UUID alone is never what a person reads).
    #[error(
        "the token for pod {0} names no pod: a multi-instance workload's certificate names its pod by name beside its UID"
    )]
    NoPodName(String),
    /// The certificate could not be read (kept as text, so the error stays `Clone` and `Eq`).
    #[error("{0}")]
    Unreadable(String),
}

impl From<PermissionExtractionError> for InstanceError {
    fn from(e: PermissionExtractionError) -> Self {
        InstanceError::Unreadable(e.to_string())
    }
}

/// Whether `uid` is a pod UID as a certificate carries it: lower-case letters, digits and `-`, at most 64, starting and
/// ending with a letter or digit. A Kubernetes UID (an RFC 4122 UUID) is one.
pub fn is_uid(uid: &str) -> bool {
    !uid.is_empty()
        && uid.len() <= 64
        && uid
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !uid.starts_with('-')
        && !uid.ends_with('-')
}

/// Whether `pod` is a pod name: a DNS subdomain (RFC 1123), lower-case letters, digits, `-` and `.`, at most 253,
/// starting and ending with a letter or digit.
pub fn is_pod_name(pod: &str) -> bool {
    let edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    !pod.is_empty()
        && pod.len() <= 253
        && pod
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
        && edge(pod.chars().next())
        && edge(pod.chars().last())
}

/// Whether `name` is in the instance names' space: any `i:` name, which only an instance's names may be.
pub fn names_instance(name: &str) -> bool {
    name.trim()
        .to_ascii_lowercase()
        .starts_with(INSTANCE_PREFIX)
}

impl Instance {
    /// The instance of pod `uid`, named `pod`; refused when either is not what a certificate can carry.
    pub fn new(uid: &str, pod: Option<&str>) -> Result<Self, InstanceError> {
        if !is_uid(uid) {
            return Err(InstanceError::InvalidUid(uid.to_string()));
        }
        if let Some(pod) = pod
            && !is_pod_name(pod)
        {
            return Err(InstanceError::InvalidPod(pod.to_string()));
        }
        Ok(Self {
            uid: uid.to_string(),
            pod: pod.map(str::to_string),
        })
    }

    /// Its names, as a certificate carries them: the UID's, then the pod's when it has one.
    pub fn san_strings(&self) -> Vec<String> {
        std::iter::once(format!("{UID_PREFIX}{}", self.uid))
            .chain(self.pod.iter().map(|pod| format!("{POD_PREFIX}{pod}")))
            .collect()
    }

    /// The instance a certificate's names say, of all its names: none when it carries no `i:` name (a single-instance
    /// workload's). Refused when an `i:` name is not one of the two, either is malformed, either is named twice, or a pod
    /// is named with no UID. Every other name is skipped.
    pub fn from_san_strings<'a>(
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<Option<Self>, InstanceError> {
        let mut uid: Option<&str> = None;
        let mut pod: Option<&str> = None;
        for name in names.into_iter().filter(|name| names_instance(name)) {
            if let Some(value) = name.strip_prefix(UID_PREFIX) {
                if uid.replace(value).is_some() {
                    return Err(InstanceError::TwoUids);
                }
            } else if let Some(value) = name.strip_prefix(POD_PREFIX) {
                if pod.replace(value).is_some() {
                    return Err(InstanceError::TwoPods);
                }
            } else {
                return Err(InstanceError::UnknownName(name.to_string()));
            }
        }
        match (uid, pod) {
            (None, None) => Ok(None),
            (None, Some(_)) => Err(InstanceError::PodWithoutUid),
            (Some(uid), pod) => Self::new(uid, pod).map(Some),
        }
    }
}

/// The instance `certificate_pem` is for, as [`Instance::from_san_strings`] reads its names.
pub fn extract_instance(certificate_pem: &str) -> Result<Option<Instance>, InstanceError> {
    let names = crate::permission::san_strings(certificate_pem)?;
    Instance::from_san_strings(names.iter().map(String::as_str))
}

/// The instance the DER certificate `der` is for: the broker reads the verified peer certificate so.
pub fn extract_instance_der(der: &[u8]) -> Result<Option<Instance>, InstanceError> {
    let names = crate::permission::san_strings_der(der)?;
    Instance::from_san_strings(names.iter().map(String::as_str))
}

/// Whether two certificates, or two sessions, of one identity are the same instance, given the pod UID each names: the
/// rule supersession is keyed by (docs/architecture/authority-model.md, "Multi-instance workloads"). They are, unless both
/// name an instance and the instances differ: a single-instance workload's are always one (a new certificate or session
/// supersedes the old, as before), a pod restarted in place (the same UID, a new key) supersedes itself, and two pods
/// of a multi-instance workload never supersede each other. A certificate that names no instance beside one that does
/// (a workload changing between one and many) supersedes it, either way.
pub fn same_instance(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const UID: &str = "8d3c0e4a-2f1b-4c7e-9a55-1f2e3d4c5b6a";

    fn certificate(names: &[&str]) -> String {
        let mut params = rcgen::CertificateParams::new(vec![
            "harness-producer.polar.serviceaccount.cluster.local".to_string(),
        ])
        .unwrap();
        for name in names {
            params
                .subject_alt_names
                .push(rcgen::SanType::URI((*name).to_string().try_into().unwrap()));
        }
        let key = rcgen::KeyPair::generate().unwrap();
        params.self_signed(&key).unwrap().pem()
    }

    #[test]
    fn an_instance_is_written_in_the_documented_form() {
        let instance = Instance::new(UID, Some("harness-producer-run-3-0-x7k2p")).unwrap();
        assert_eq!(
            instance.san_strings(),
            [
                format!("i:uid:{UID}"),
                "i:pod:harness-producer-run-3-0-x7k2p".to_string()
            ]
        );
        assert_eq!(
            Instance::new(UID, None).unwrap().san_strings(),
            [format!("i:uid:{UID}")]
        );
    }

    #[test]
    fn a_certificate_with_no_instance_names_is_one_instance_s() {
        let pem = certificate(&["p:pub:news", "g:cassini-client"]);
        assert_eq!(extract_instance(&pem).unwrap(), None);
    }

    #[test]
    fn a_certificate_s_instance_is_read_from_its_uri_names_and_nothing_else() {
        let pem = certificate(&[
            "p:pub:news",
            &format!("i:uid:{UID}"),
            "g:cassini-client",
            "i:pod:build-job-42-abcde",
        ]);
        assert_eq!(
            extract_instance(&pem).unwrap(),
            Some(Instance::new(UID, Some("build-job-42-abcde")).unwrap())
        );
    }

    #[test]
    fn instance_names_that_do_not_read_as_one_instance_are_refused() {
        let uid = format!("i:uid:{UID}");
        let cases: [(&[&str], InstanceError); 7] = [
            (&[&uid, "i:uid:another"], InstanceError::TwoUids),
            (&[&uid, "i:pod:a", "i:pod:b"], InstanceError::TwoPods),
            (&["i:pod:a"], InstanceError::PodWithoutUid),
            (
                &["i:node:n1"],
                InstanceError::UnknownName("i:node:n1".into()),
            ),
            (
                &["i:uid:Not_A_Uid"],
                InstanceError::InvalidUid("Not_A_Uid".into()),
            ),
            (
                &[&uid, "i:pod:-leading"],
                InstanceError::InvalidPod("-leading".into()),
            ),
            (&["i:uid:"], InstanceError::InvalidUid(String::new())),
        ];
        for (names, expected) in cases {
            assert_eq!(
                Instance::from_san_strings(names.iter().copied()),
                Err(expected.clone()),
                "{names:?}"
            );
        }
        // From a certificate too.
        let pem = certificate(&[&uid, "i:uid:another"]);
        assert_eq!(extract_instance(&pem), Err(InstanceError::TwoUids));
    }

    #[test]
    fn a_name_in_the_instance_space_is_recognised_whatever_its_case() {
        assert!(names_instance("i:uid:x"));
        assert!(names_instance(" I:POD:x"));
        assert!(!names_instance("p:pub:i:x"));
        assert!(!names_instance("g:i"));
    }

    #[test]
    fn supersession_is_by_instance_only_when_both_name_one() {
        assert!(same_instance(None, None));
        assert!(same_instance(Some("a"), Some("a")));
        assert!(!same_instance(Some("a"), Some("b")));
        assert!(same_instance(Some("a"), None));
        assert!(same_instance(None, Some("b")));
    }

    fn uid() -> impl Strategy<Value = String> {
        "[a-z0-9]([a-z0-9-]{0,20}[a-z0-9])?"
    }

    fn pod() -> impl Strategy<Value = String> {
        "[a-z0-9]([a-z0-9.-]{0,30}[a-z0-9])?"
    }

    proptest! {
        /// Any instance reads back as written, among any other names, in any order.
        #[test]
        fn an_instance_reads_back_as_written(
            uid in uid(),
            pod in proptest::option::of(pod()),
            others in proptest::collection::vec("[pg]:[a-z]{1,8}", 0..4),
            rotate in 0usize..8,
        ) {
            let instance = Instance::new(&uid, pod.as_deref()).unwrap();
            let mut names = instance.san_strings();
            names.extend(others);
            let len = names.len();
            names.rotate_left(rotate % len);
            prop_assert_eq!(
                Instance::from_san_strings(names.iter().map(String::as_str)).unwrap(),
                Some(instance)
            );
        }

        /// A UID or a pod name with a character a certificate's name cannot carry here is refused, never read as some
        /// other instance.
        #[test]
        fn a_malformed_uid_or_pod_name_is_refused(
            good in uid(),
            bad in "[a-z0-9]{0,4}[A-Z_ /:@*][a-z0-9]{0,4}",
        ) {
            let uid_name = format!("{UID_PREFIX}{bad}");
            prop_assert!(Instance::from_san_strings([uid_name.as_str()]).is_err());
            let good_name = format!("{UID_PREFIX}{good}");
            let pod_name = format!("{POD_PREFIX}{bad}");
            prop_assert!(
                Instance::from_san_strings([good_name.as_str(), pod_name.as_str()]).is_err()
            );
        }

        /// Two UIDs are refused, whatever they are and wherever they stand: a certificate is one pod's.
        #[test]
        fn two_uids_are_refused(a in uid(), b in uid(), pod in proptest::option::of(pod())) {
            let mut names = vec![format!("{UID_PREFIX}{a}"), format!("{UID_PREFIX}{b}")];
            names.extend(pod.map(|p| format!("{POD_PREFIX}{p}")));
            prop_assert_eq!(
                Instance::from_san_strings(names.iter().map(String::as_str)),
                Err(InstanceError::TwoUids)
            );
        }

        /// Supersession by instance: two named instances are one exactly when their UIDs are equal, and an unnamed one
        /// is the same as any; the rule is symmetric.
        #[test]
        fn the_supersession_rule_is_symmetric_and_keys_on_the_uid(
            a in proptest::option::of(uid()),
            b in proptest::option::of(uid()),
        ) {
            let same = same_instance(a.as_deref(), b.as_deref());
            prop_assert_eq!(same, same_instance(b.as_deref(), a.as_deref()));
            match (&a, &b) {
                (Some(a), Some(b)) => prop_assert_eq!(same, a == b),
                _ => prop_assert!(same),
            }
        }
    }
}
