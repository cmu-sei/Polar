//! The plans committed in the repository are plans this tool can read and would run: a change to
//! the schema in `plan.dhall` or to this tool that breaks either shows up here, not on a cluster.

use polar_cli::plan::Plan;

fn read(text: &str) -> Plan {
    Plan::from_yaml(text).expect("a plan")
}

const CORE: &str = include_str!("../../../deploy/local/core/rendered/plan.yaml");

#[test]
fn the_core_plan_is_sound_and_has_the_stages_the_core_needs() {
    let plan = read(CORE);
    assert_eq!(plan.problems(), Vec::<String>::new());

    let names: Vec<_> = plan.stages.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["operator", "issuer", "broker", "credentials", "graph"]);
    assert!(plan.stages[0].install_crd, "the first stage installs the operator and its CRD");
    assert!(plan.stages.iter().all(|s| s.shared), "everything in the core is shared");
    assert!(plan.stages.iter().all(|s| !s.optional), "the core is not optional");

    // Nothing is left out: all of it runs by default.
    let default: Vec<_> = plan.select(&[], &[]).unwrap().iter().map(|s| s.name.clone()).collect();
    assert_eq!(default, names);
}

#[test]
fn the_cores_graph_secrets_are_made_together() {
    let plan = read(CORE);
    let grouped: Vec<_> = plan
        .stages
        .iter()
        .flat_map(|s| &s.secrets)
        .filter(|s| s.group.as_deref() == Some("graph"))
        .collect();
    assert_eq!(grouped.len(), 2, "the bare password and the graph's own form are one group");
}

#[test]
fn the_core_runs_nothing_but_the_operator_as_a_plain_workload() {
    let plan = read(CORE);
    let mut workloads = Vec::new();
    for object in plan.objects() {
        if matches!(object.kind(), Some("Deployment" | "StatefulSet" | "Job" | "DaemonSet" | "CronJob")) {
            workloads.push(object.id().unwrap().to_string());
        }
    }
    assert_eq!(workloads, ["Deployment polar/polar-operator"], "everything else is a PolarAgent");
}

#[test]
fn the_same_core_in_two_plans_is_one_core() {
    // Two applications include the same core: the shared stages are identical, so they run once.
    assert_eq!(read(CORE).stages, read(CORE).stages);
}

#[test]
fn the_example_plan_is_sound() {
    let plan = read(include_str!("../../../deploy/examples/plan.yaml"));
    assert_eq!(plan.problems(), Vec::<String>::new());
}

#[test]
fn nothing_in_a_committed_plan_looks_like_a_secret_value() {
    // A literal in a plan is read by anyone who reads the plan. The core and example plans make
    // their secrets at run time; none should carry one.
    for text in [CORE, include_str!("../../../deploy/examples/plan.yaml")] {
        let plan = read(text);
        let described = plan.describe();
        assert!(!described.contains("literal"), "{described}");
    }
}
