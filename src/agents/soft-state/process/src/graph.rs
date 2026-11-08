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

//! What the writer writes: one `(:AgentHealth {workload})` for each expected workload, its own label and no other
//! (docs/architecture/soft-state.md, "Self-state and introspection"). It holds what the last round found (`state`,
//! `observed_at_ms`), the latest report the workload gave (its status, phase, reason, message, connections, counters
//! and uptime, with `answered_at_ms`; an unresponsive round leaves them as the last answer said), and the last
//! transitions, each a JSON string, oldest first.

use crate::classify::Observation;
use crate::expected::Entry;
use crate::history::{History, Transition};
use polar::graph::controller::{DynamicNodeKey, GraphNodeKey, GraphOp, GraphValue, Property};
use std::collections::BTreeMap;

/// The one label the writer writes.
pub const LABEL: &str = "AgentHealth";

/// What is read back at start: every workload's transitions.
pub const READ_BACK: &str =
    "MATCH (h:AgentHealth) RETURN h.workload AS workload, h.transitions AS transitions";

fn text(value: &str) -> GraphValue {
    GraphValue::String(value.to_string())
}

fn number(value: u64) -> GraphValue {
    GraphValue::I64(i64::try_from(value).unwrap_or(i64::MAX))
}

fn optional(value: Option<&str>) -> GraphValue {
    value.map_or(GraphValue::Null, text)
}

/// The node of `entry`, and its properties for what the round at `at_ms` found and the history after it.
pub fn node_of(
    entry: &Entry,
    observation: &Observation,
    history: &History,
    at_ms: i64,
) -> Result<(Box<dyn GraphNodeKey>, Vec<Property>), String> {
    let key = DynamicNodeKey::new(LABEL, "workload", &entry.workload)
        .map_err(|e| format!("the node of {} cannot be named: {e:?}", entry.workload))?;
    let transitions = history
        .transitions()
        .map(|t| {
            serde_json::to_string(t)
                .map(GraphValue::String)
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut props = vec![
        Property("identity".into(), text(&entry.identity)),
        Property("state".into(), text(observation.state())),
        Property("observed_at_ms".into(), GraphValue::I64(at_ms)),
        Property("transitions".into(), GraphValue::List(transitions)),
    ];
    if let Observation::Answering(report) = observation {
        let phase = report.phase.to_string();
        props.extend([
            Property("answered_at_ms".into(), GraphValue::I64(at_ms)),
            Property("status".into(), text(report.status.as_str())),
            Property("phase".into(), text(&phase)),
            Property("reason".into(), optional(report.reason.map(|r| r.as_str()))),
            Property("message".into(), optional(report.message.as_deref())),
            Property(
                "cassini_connected".into(),
                GraphValue::Bool(report.cassini_connected),
            ),
            Property(
                "graph_connected".into(),
                GraphValue::Bool(report.graph_connected),
            ),
            Property(
                "expects_graph".into(),
                GraphValue::Bool(report.expects_graph),
            ),
            Property("uptime_secs".into(), number(report.uptime_secs)),
            Property(
                "cassini_disconnects".into(),
                number(report.counters.cassini_disconnects),
            ),
            Property(
                "graph_op_failures".into(),
                number(report.counters.graph_op_failures),
            ),
            Property(
                "refused_topics".into(),
                number(report.counters.refused_topics),
            ),
            Property(
                "health_file_write_failures".into(),
                number(report.counters.health_file_write_failures),
            ),
        ]);
    }
    Ok((Box::new(key), props))
}

/// One write for a whole round.
pub fn round_op(items: Vec<(Box<dyn GraphNodeKey>, Vec<Property>)>) -> GraphOp {
    GraphOp::UpsertNodes { items }
}

/// The histories the graph holds, by workload, from the rows of [`READ_BACK`]. A transition that does not read is
/// left out: the history is a hint, and the next transitions are written whole.
pub fn histories_of(rows: &[neo4rs::Row]) -> BTreeMap<String, History> {
    rows.iter()
        .filter_map(|row| {
            let workload = row.get::<String>("workload").ok()?;
            let stored = row
                .get::<Option<Vec<String>>>("transitions")
                .ok()
                .flatten()
                .unwrap_or_default()
                .iter()
                .filter_map(|t| serde_json::from_str::<Transition>(t).ok())
                .collect();
            Some((workload, History::from_stored(stored)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar_health::{HealthCounters, HealthPhase, HealthReason, HealthReport, ReportStatus};

    fn entry() -> Entry {
        Entry {
            workload: "jira-observer".into(),
            identity: "jira-observer.polar.serviceaccount.cluster.local".into(),
        }
    }

    fn names(props: &[Property]) -> Vec<&str> {
        props.iter().map(|p| p.0.as_str()).collect()
    }

    #[test]
    fn an_answer_writes_the_report_and_silence_writes_only_what_the_round_found() {
        let mut history = History::default();
        history.record(1, "unhealthy", Some("configuration"));
        let report = HealthReport {
            status: ReportStatus::Unhealthy,
            phase: HealthPhase::Normal,
            reason: Some(HealthReason::Configuration),
            message: Some("JIRA_URL is still the example placeholder".into()),
            cassini_connected: true,
            graph_connected: false,
            expects_graph: false,
            counters: HealthCounters::default(),
            uptime_secs: 61,
        };
        let (_, props) = node_of(&entry(), &Observation::Answering(report), &history, 1).unwrap();
        let named = names(&props);
        for name in [
            "state",
            "status",
            "reason",
            "message",
            "transitions",
            "answered_at_ms",
        ] {
            assert!(named.contains(&name), "{name} in {named:?}");
        }
        let state = props.iter().find(|p| p.0 == "state").unwrap();
        assert!(matches!(&state.1, GraphValue::String(s) if s == "unhealthy"));

        let (_, props) = node_of(&entry(), &Observation::Unresponsive, &history, 2).unwrap();
        let named = names(&props);
        assert!(!named.contains(&"status") && !named.contains(&"answered_at_ms"));
        let state = props.iter().find(|p| p.0 == "state").unwrap();
        assert!(matches!(&state.1, GraphValue::String(s) if s == "unresponsive"));
        let transitions = props.iter().find(|p| p.0 == "transitions").unwrap();
        match &transitions.1 {
            GraphValue::List(items) => {
                assert_eq!(items.len(), 1);
                let GraphValue::String(json) = &items[0] else {
                    panic!("a transition is a string")
                };
                let back: Transition = serde_json::from_str(json).unwrap();
                assert_eq!(back.state, "unhealthy");
                assert_eq!(back.reason.as_deref(), Some("configuration"));
            }
            other => panic!("the transitions are a list: {other:?}"),
        }
    }
}
