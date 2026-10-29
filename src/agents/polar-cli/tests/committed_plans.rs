//! The plans committed in the repository are plans this tool can read and would run: a change to
//! the schema in `plan.dhall` or to this tool that breaks either shows up here, not on a cluster.

use polar_cli::plan::Plan;

fn read(text: &str) -> Plan {
    Plan::from_yaml(text).expect("a plan")
}

#[test]
fn the_platform_plan_is_sound_and_has_the_stages_the_platform_needs() {
    let plan = read(include_str!("../../../deploy/local/fork/rendered/plan.yaml"));
    assert_eq!(plan.problems(), Vec::<String>::new());

    let names: Vec<_> = plan.stages.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["operator", "issuer", "platform", "graph"]);
    assert!(plan.stages[0].install_crd, "the operator stage installs the CRD");
    assert!(plan.stages[3].optional, "the graph is optional");

    // By default the graph is not run; asked for, it is.
    let default: Vec<_> = plan.select(&[], &[]).unwrap().iter().map(|s| s.name.clone()).collect();
    assert_eq!(default, ["operator", "issuer", "platform"]);
    let with: Vec<_> = plan.select(&["graph".into()], &[]).unwrap().iter().map(|s| s.name.clone()).collect();
    assert_eq!(with, ["operator", "issuer", "platform", "graph"]);
}

#[test]
fn the_platform_plans_graph_secrets_are_made_together() {
    let plan = read(include_str!("../../../deploy/local/fork/rendered/plan.yaml"));
    let issuer = &plan.stages[1];
    let grouped: Vec<_> = issuer.secrets.iter().filter(|s| s.group.as_deref() == Some("graph")).collect();
    assert_eq!(grouped.len(), 2, "the bare password and the graph's own form are one group");
}

#[test]
fn the_example_plan_is_sound() {
    let plan = read(include_str!("../../../deploy/examples/plan.yaml"));
    assert_eq!(plan.problems(), Vec::<String>::new());
}

#[test]
fn nothing_in_a_committed_plan_looks_like_a_secret_value() {
    // A literal in a plan is read by anyone who reads the plan. The platform and example plans make
    // their secrets at run time; none should carry one.
    for text in [
        include_str!("../../../deploy/local/fork/rendered/plan.yaml"),
        include_str!("../../../deploy/examples/plan.yaml"),
    ] {
        let plan = read(text);
        let described = plan.describe();
        assert!(!described.contains("literal"), "{described}");
    }
}
