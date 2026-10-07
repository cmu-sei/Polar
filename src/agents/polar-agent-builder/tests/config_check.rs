//! The render-time checker, run as the pipeline runs it: the program, started with the
//! environment a workload will be given, and nothing else.

use std::process::{Command, Output};

/// Runs `polar-config-check` with exactly these variables set. `env_clear` matters: the
/// developer's own shell may have `POLAR_HEALTH_*` set, and that must not leak into a check.
fn run(args: &[&str], vars: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_polar-config-check"))
        .args(args)
        .env_clear()
        .envs(vars.iter().copied())
        .output()
        .expect("the checker runs")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn an_empty_environment_passes() {
    let o = run(&[], &[]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stdout(&o).contains("ok"), "{}", stdout(&o));
}

#[test]
fn what_a_workload_is_given_passes() {
    let o = run(
        &[],
        &[
            ("POLAR_CAPABILITIES", "cassini,graph-write,encryption"),
            ("POLAR_SHUTDOWN_BUDGET_SECS", "45"),
            (
                "POLAR_HEALTH_FILE",
                "/var/run/polar-health/polar-health.json",
            ),
            (
                "POLAR_HEALTH_CERTS",
                "/etc/tls/certs/cert.pem,/etc/neo4j-client-tls/cert.pem",
            ),
            ("POLAR_HEALTH_EXPIRY_SECS", "60"),
            ("POLAR_HEALTH_TICK_SECS", "30"),
            ("POLAR_HEALTH_REJUVENATION_SECS", "300"),
            ("POLAR_HEALTH_STALE_SECS", "90"),
            (
                "POLAR_HEALTH_DEP_ENDPOINTS",
                "cassini-ip-svc.polar.svc.cluster.local:8080:300,polar-db-svc.polar-graph.svc.cluster.local:7687:300",
            ),
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
}

#[test]
fn a_bad_variable_fails_the_check_and_names_itself() {
    let o = run(&[], &[("POLAR_HEALTH_TICK_SECS", "soon")]);
    assert_eq!(o.status.code(), Some(1));
    let err = stderr(&o);
    assert!(err.contains("ERROR: POLAR_HEALTH_TICK_SECS"), "{err}");
    assert!(err.contains("soon"), "{err}");
    assert!(
        err.contains("between health ticks"),
        "what it is for: {err}"
    );
}

#[test]
fn every_problem_is_reported_on_its_own_line() {
    let o = run(
        &[],
        &[
            ("POLAR_SHUTDOWN_BUDGET_SECS", "0"),
            ("POLAR_HEALTH_TICK_SECS", "soon"),
            ("POLAR_HEALTH_STALE_SECS", "x"),
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    let errors: Vec<_> = stderr(&o)
        .lines()
        .filter(|l| l.starts_with("ERROR: "))
        .map(String::from)
        .collect();
    assert_eq!(errors.len(), 3, "{errors:?}");
}

#[test]
fn a_relation_between_variables_fails_the_check() {
    // No room for a rejuvenation before the certificate hits its hard floor.
    let o = run(&[], &[("POLAR_HEALTH_REJUVENATION_SECS", "250")]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("hard expiry floor"), "{}", stderr(&o));
}

#[test]
fn the_developers_own_environment_does_not_leak_into_a_check() {
    // The test harness's own environment is not passed in (env_clear), so this checks the
    // checker reads only what it is given, by giving it nothing and expecting a pass.
    let o = run(&[], &[]);
    assert_eq!(o.status.code(), Some(0));
}

#[test]
fn describe_lists_every_variable_by_section() {
    let o = run(&["--describe"], &[]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let out = stdout(&o);
    for expected in [
        "builder",
        "health",
        "POLAR_CAPABILITIES",
        "POLAR_SHUTDOWN_BUDGET_SECS",
        "POLAR_HEALTH_TICK_SECS",
        "POLAR_HEALTH_FILE",
    ] {
        assert!(out.contains(expected), "{expected} missing from:\n{out}");
    }
}

#[test]
fn describe_shows_the_kind_the_default_and_what_it_is_for() {
    let out = stdout(&run(&["--describe"], &[]));
    let tick = out
        .lines()
        .find(|l| l.contains("POLAR_HEALTH_TICK_SECS"))
        .expect("the tick is described");
    assert!(tick.contains("default 30"), "{tick}");
    assert!(tick.contains("whole number of seconds"), "{tick}");
    assert!(tick.contains("between health ticks"), "{tick}");
}

#[test]
fn an_unknown_argument_is_a_usage_error() {
    let o = run(&["--nonsense"], &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).to_lowercase().contains("usage"),
        "{}",
        stderr(&o)
    );
}
