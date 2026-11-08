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
use rkyv::{from_bytes, to_bytes};

#[test]
fn test_serde_git_schedule_change_create() {
    let change = GitScheduleChange::Create {
        path: "schedules/worker.yaml".to_string(),
        json: r#"{"id": "1"}"#.to_string(),
    };
    let json = serde_json::to_string(&change).unwrap();
    let roundtrip: GitScheduleChange = serde_json::from_str(&json).unwrap();
    assert_eq!(change, roundtrip);
}

#[test]
fn test_serde_git_schedule_change_update() {
    let change = GitScheduleChange::Update {
        path: "schedules/worker.yaml".to_string(),
        json: r#"{"id": "2"}"#.to_string(),
    };
    let json = serde_json::to_string(&change).unwrap();
    let roundtrip: GitScheduleChange = serde_json::from_str(&json).unwrap();
    assert_eq!(change, roundtrip);
}

#[test]
fn test_serde_git_schedule_change_delete() {
    let change = GitScheduleChange::Delete {
        path: "schedules/worker.yaml".to_string(),
    };
    let json = serde_json::to_string(&change).unwrap();
    let roundtrip: GitScheduleChange = serde_json::from_str(&json).unwrap();
    assert_eq!(change, roundtrip);
}

#[test]
fn test_rkyv_git_schedule_change_create() {
    let change = GitScheduleChange::Create {
        path: "schedules/worker.yaml".to_string(),
        json: r#"{"id": "1"}"#.to_string(),
    };
    let bytes = to_bytes::<rkyv::rancor::Error>(&change).unwrap();
    let roundtrip: GitScheduleChange =
        from_bytes::<GitScheduleChange, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(change, roundtrip);
}

#[test]
fn test_rkyv_git_schedule_change_update() {
    let change = GitScheduleChange::Update {
        path: "schedules/worker.yaml".to_string(),
        json: r#"{"id": "2"}"#.to_string(),
    };
    let bytes = to_bytes::<rkyv::rancor::Error>(&change).unwrap();
    let roundtrip: GitScheduleChange =
        from_bytes::<GitScheduleChange, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(change, roundtrip);
}

#[test]
fn test_rkyv_git_schedule_change_delete() {
    let change = GitScheduleChange::Delete {
        path: "schedules/worker.yaml".to_string(),
    };
    let bytes = to_bytes::<rkyv::rancor::Error>(&change).unwrap();
    let roundtrip: GitScheduleChange =
        from_bytes::<GitScheduleChange, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(change, roundtrip);
}
