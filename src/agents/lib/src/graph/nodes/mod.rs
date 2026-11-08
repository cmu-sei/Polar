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

pub mod agents;
pub mod builds;
pub mod git;
pub mod gitlab;
pub mod kube;
pub mod schedule;

/// Every key of every vocabulary here keeps the rules its patterns are composed by (`pattern_problem`): it binds the
/// variable it is given, `State` included, whose unbound `(:State)` made an empty node at every transition.
#[cfg(test)]
mod pattern_tests {
    use super::builds::{ArtifactNodeKey, BackendJobIdentity, BuildNodeKey};
    use super::git::GitNodeKey;
    use super::gitlab::GitlabNodeKey;
    use super::kube::KubeNodeKey;
    use super::schedule::ScheduleKey;
    use crate::graph::controller::{
        DynamicNodeKey, GraphNodeKey, NodeKeyVocabulary, pattern_problem, shapes_of,
    };
    use proptest::prelude::*;

    fn boxed<V: NodeKeyVocabulary + 'static>() -> Vec<Box<dyn GraphNodeKey>> {
        V::every_variant()
            .into_iter()
            .map(|k| Box::new(k) as Box<dyn GraphNodeKey>)
            .collect()
    }

    fn every_key() -> Vec<Box<dyn GraphNodeKey>> {
        let mut keys = boxed::<KubeNodeKey>();
        keys.extend(boxed::<BuildNodeKey>());
        keys.extend(boxed::<ArtifactNodeKey>());
        keys.extend(boxed::<GitlabNodeKey>());
        keys.extend(boxed::<GitNodeKey>());
        keys.extend(boxed::<ScheduleKey>());
        keys
    }

    /// Each vocabulary lists each of its variants once.
    fn distinct<V: NodeKeyVocabulary>() -> bool {
        let all = V::every_variant();
        let mut seen: Vec<String> = all
            .iter()
            .map(|k| format!("{:?}", std::mem::discriminant(k)))
            .collect();
        seen.sort();
        seen.dedup();
        seen.len() == all.len()
    }

    #[test]
    fn every_vocabulary_lists_each_variant_once() {
        assert!(distinct::<KubeNodeKey>());
        assert!(distinct::<BuildNodeKey>());
        assert!(distinct::<ArtifactNodeKey>());
        assert!(distinct::<GitlabNodeKey>());
        assert!(distinct::<GitNodeKey>());
        assert!(distinct::<ScheduleKey>());
    }

    #[test]
    fn the_state_anchor_binds_its_variable() {
        assert_eq!(KubeNodeKey::State.cypher_match("stype").0, "(stype:State)");
        assert_eq!(BuildNodeKey::State.cypher_match("stype").0, "(stype:State)");
    }

    /// A state instance's key properties do not depend on the op that writes it.
    #[test]
    fn a_pod_state_is_keyed_on_its_pod_and_its_time_whatever_the_variable() {
        let key = KubeNodeKey::PodState {
            pod_uid: "u".into(),
            valid_from: 1,
            cluster_uid: "c".into(),
        };
        assert_eq!(
            key.cypher_match("sinst").0,
            "(sinst:PodState { pod_uid: $sinst_pod_uid, valid_from: $sinst_valid_from, cluster_uid: $sinst_cluster_uid })"
        );
    }

    /// The shapes a vocabulary declares are its fixed-label variants', each once: a backend job's label is known only
    /// at run time, so it has none.
    #[test]
    fn a_runtime_label_has_no_shape() {
        let shapes = shapes_of::<BuildNodeKey>();
        assert!(shapes.iter().all(|s| s.labels != ["GitlabRunnerJob"]));
        assert!(shapes.iter().any(|s| s.labels == ["BuildJob"]));
    }

    proptest! {
        #[test]
        fn every_key_binds_the_variable_it_is_given(prefix in "[a-z][a-z0-9]{0,5}") {
            for key in every_key() {
                prop_assert_eq!(pattern_problem(key.as_ref(), &prefix), None);
            }
        }

        #[test]
        fn a_key_made_at_run_time_binds_its_variable_too(
            prefix in "[a-z][a-z0-9]{0,5}",
            label in "[A-Z][A-Za-z0-9_]{0,8}",
            props in prop::collection::btree_set("[a-z][a-z0-9_]{0,6}", 1..4),
            value in ".{0,12}",
        ) {
            // A keyword is refused at construction; what is made keeps the rules.
            if let Ok(key) = DynamicNodeKey::new(&label, props.iter().next().unwrap(), &value) {
                prop_assert_eq!(pattern_problem(&key, &prefix), None);
            }
            let identity = props.iter().map(|p| (p.clone(), value.clone())).collect();
            if let Ok(identity) = BackendJobIdentity::new(&label, identity) {
                let key = BuildNodeKey::BackendJob(identity);
                prop_assert_eq!(pattern_problem(&key, &prefix), None);
            }
        }
    }
}
