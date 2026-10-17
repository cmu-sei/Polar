//! Findings: facts about packages and artifacts, reported in a run's context.
//!
//! Reported by scanners (`scanning.nu`: cargo-audit and friends) and by
//! analyzers run by the build orchestrator.
//!
//! ```text
//! (BuildJob)-[:REPORTED {at}]->(SecurityAdvisory)-[:IS]->(Finding)
//!                               (SecurityAdvisory)-[:AFFECTS {at}]->(Package)
//! (BuildJob)-[:REPORTED {at}]->(PackageStatus)-[:IS]->(Finding)
//!                               (PackageStatus)-[:CONCERNS {at}]->(Package)
//! (BuildJob)-[:REPORTED {at}]->(AnalysisFinding)-[:IS]->(Finding)
//! ```
//!
//! Each event is one RawQuery, so each finding is written atomically. Edges
//! are MERGEd on their endpoints and `at` is set only on creation, so a
//! replayed event never adds a second edge.

use neo4rs::BoltType;
use polar::graph::controller::GraphOp;
use polar::{AnalysisFindingPayload, PackageStatusFoundPayload, SecurityAdvisoryFoundPayload};
use tracing::trace;

/// Collect `(key, value)` pairs into the map a Cypher `SET n += $props` takes.
/// Absent optionals are left out rather than written as nulls.
fn props_map(props: Vec<(&str, BoltType)>) -> BoltType {
    BoltType::Map(props.into_iter().map(|(k, v)| (k.into(), v)).collect())
}

fn push_present(
    props: &mut Vec<(&'static str, BoltType)>,
    fields: Vec<(&'static str, Option<String>)>,
) {
    for (key, value) in fields {
        if let Some(value) = value {
            props.push((key, value.into()));
        }
    }
}

/// A known vulnerability or soundness advisory (RUSTSEC, GHSA, ...).
///
/// The AFFECTS edge is only written when the scanner's (name, version)
/// resolved to an SBOM purl in scanning.nu. Writing it with a null purl would
/// MERGE a garbage `Package {purl: null}` node.
pub fn security_advisory(p: SecurityAdvisoryFoundPayload, observed_at: &str) -> Vec<GraphOp> {
    trace!("SecurityAdvisoryFound: {} ({})", p.identifier, p.severity);

    let mut cypher = String::from(
        "MERGE (a:SecurityAdvisory { identifier: $identifier }) \
         SET a += $props \
         MERGE (f:Finding { type: \"Finding\" }) \
         MERGE (a)-[:IS]->(f) \
         MERGE (j:BuildJob { build_id: $build_id }) \
         MERGE (j)-[r:REPORTED]->(a) \
         ON CREATE SET r.at = $observed_at",
    );

    let mut props: Vec<(&str, BoltType)> = vec![
        ("identifier", p.identifier.clone().into()),
        ("severity", p.severity.into()),
    ];
    // NB: fix_version carries a semver constraint (">=x"), not a concrete
    // version; it is stored under the wire field's name.
    push_present(
        &mut props,
        vec![
            ("kind", p.kind),
            ("scanner", p.scanner),
            ("cve_id", p.cve_id),
            ("ghsa_id", p.ghsa_id),
            ("fix_version", p.fix_version),
            ("unaffected_constraint", p.unaffected_constraint),
            ("advisory_url", p.advisory_url),
        ],
    );

    let mut params: Vec<(String, BoltType)> = vec![
        ("identifier".into(), p.identifier.into()),
        ("build_id".into(), p.build_id.into()),
        ("observed_at".into(), observed_at.into()),
        ("props".into(), props_map(props)),
    ];
    if let Some(purl) = p.affected_package {
        cypher.push_str(
            " MERGE (p:Package { purl: $affected_purl }) \
              MERGE (a)-[af:AFFECTS]->(p) \
              ON CREATE SET af.at = $observed_at",
        );
        params.push(("affected_purl".into(), purl.into()));
    }
    vec![GraphOp::RawQuery { cypher, params }]
}

/// A state-of-being fact about a package rather than a flaw: cargo-audit's
/// `unmaintained` today. Same shape as an advisory, but the package edge is
/// CONCERNS, not AFFECTS, and there is no severity.
pub fn package_status(p: PackageStatusFoundPayload, observed_at: &str) -> Vec<GraphOp> {
    trace!("PackageStatusFound: {} ({})", p.identifier, p.kind);

    let mut cypher = String::from(
        "MERGE (s:PackageStatus { identifier: $identifier }) \
         SET s += $props \
         MERGE (f:Finding { type: \"Finding\" }) \
         MERGE (s)-[:IS]->(f) \
         MERGE (j:BuildJob { build_id: $build_id }) \
         MERGE (j)-[r:REPORTED]->(s) \
         ON CREATE SET r.at = $observed_at",
    );

    let mut props: Vec<(&str, BoltType)> = vec![
        ("identifier", p.identifier.clone().into()),
        ("kind", p.kind.into()),
        ("package_name", p.package_name.into()),
        ("package_version", p.package_version.into()),
    ];
    push_present(
        &mut props,
        vec![("scanner", p.scanner), ("advisory_url", p.advisory_url)],
    );

    let mut params: Vec<(String, BoltType)> = vec![
        ("identifier".into(), p.identifier.into()),
        ("build_id".into(), p.build_id.into()),
        ("observed_at".into(), observed_at.into()),
        ("props".into(), props_map(props)),
    ];
    if let Some(purl) = p.affected_package {
        cypher.push_str(
            " MERGE (p:Package { purl: $affected_purl }) \
              MERGE (s)-[c:CONCERNS]->(p) \
              ON CREATE SET c.at = $observed_at",
        );
        params.push(("affected_purl".into(), purl.into()));
    }
    vec![GraphOp::RawQuery { cypher, params }]
}

/// One finding from an analyzer run.
///
/// It carries no package: the finding belongs to its run (`build_id`), and the
/// run's ANALYZED edge (see `execution::analysis_started`) says what was
/// analyzed, so an analyzer can't attribute a finding to a package it wasn't
/// given. Within a run a finding is identified by (rule, location).
pub fn analysis_finding(p: AnalysisFindingPayload, observed_at: &str) -> Vec<GraphOp> {
    trace!("AnalysisFindingReported: {} in {}", p.rule_id, p.build_id);

    let location = match (&p.path, p.line) {
        (Some(path), Some(line)) => format!("{path}:{line}"),
        (Some(path), None) => path.clone(),
        (None, _) => String::new(),
    };

    let mut props: Vec<(&str, BoltType)> = vec![
        ("rule_id", p.rule_id.clone().into()),
        ("message", p.message.into()),
    ];
    push_present(&mut props, vec![("severity", p.severity), ("path", p.path)]);
    if let Some(line) = p.line {
        props.push(("line", BoltType::from(line as i64)));
    }

    let cypher = "MERGE (j:BuildJob { build_id: $build_id }) \
         MERGE (f:AnalysisFinding { build_id: $build_id, rule_id: $rule_id, location: $location }) \
         SET f += $props \
         MERGE (t:Finding { type: \"Finding\" }) \
         MERGE (f)-[:IS]->(t) \
         MERGE (j)-[r:REPORTED]->(f) \
         ON CREATE SET r.at = $observed_at"
        .to_string();
    let params = vec![
        ("build_id".into(), p.build_id.into()),
        ("rule_id".into(), p.rule_id.into()),
        ("location".into(), location.into()),
        ("observed_at".into(), observed_at.into()),
        ("props".into(), props_map(props)),
    ];
    vec![GraphOp::RawQuery { cypher, params }]
}
