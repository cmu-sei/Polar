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

use polar_scheduler_common::GitScheduleChange;
use proptest::prelude::*;

proptest! {
    #[test]
    fn prop_git_change_create_rkyv_roundtrip(
        path in ".+",
        json in ".*"
    ) {
        let original = GitScheduleChange::Create {
            path: path.clone(),
            json: json.clone(),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let decoded = rkyv::from_bytes::<GitScheduleChange, rkyv::rancor::Error>(&bytes).unwrap();
        prop_assert_eq!(
            serde_json::to_string(&original).unwrap(),
            serde_json::to_string(&decoded).unwrap()
        );
    }

    #[test]
    fn prop_git_change_delete_rkyv_roundtrip(path in ".+") {
        let original = GitScheduleChange::Delete { path };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
        let decoded = rkyv::from_bytes::<GitScheduleChange, rkyv::rancor::Error>(&bytes).unwrap();
        prop_assert_eq!(
            serde_json::to_string(&original).unwrap(),
            serde_json::to_string(&decoded).unwrap()
        );
    }
}
