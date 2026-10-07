//! polar-config-check
//!
//! Judges an environment the way an agent will judge its own when it starts, so a bad one
//! fails a render instead of a rollout. It reads the process environment, which the caller
//! sets to what a workload will be given:
//!
//! ```text
//! with-env $the_workloads_env { ^polar-config-check }
//! ```
//!
//! and runs the one function the agent builder runs at startup, so there is no second copy
//! of any rule to drift. Every problem is printed on its own line to stderr, and the exit
//! status is 0 if there were none, 1 if there were, 2 for a usage error.
//!
//! `--describe` instead prints every variable the agent reads, by section, with its kind,
//! default and what it is for. `--describe --json` prints the same as one JSON document, for
//! tools that scaffold a workload entry or a schedule, or validate one: see
//! `environment::describe_json` for its shape.
//!
//! `--effective` instead shows what the environment resolves to: every variable of the
//! sections that apply, its value (a secret is shown as `<hidden>`), where it came from
//! (the environment or a default) and whether a scheduled override may change it. Only
//! declared variables are shown, as only declared variables ever reach an agent. Problems are
//! still reported, and still fail. `--effective --json` is the same as one JSON document.
// Reading the process environment goes through polar-env (see clippy.toml).
#![deny(clippy::disallowed_methods)]

use polar_agent_builder::environment;
use polar_env::ProcessEnv;
use std::process::ExitCode;

const USAGE: &str = "usage: polar-config-check [--describe | --effective] [--json]\n\n\
    Checks the environment of this process the way an agent built by polar-agent-builder\n\
    checks its own when it starts. With --describe, lists the variables instead, and with\n\
    --json as one JSON document. With --effective, shows what each variable resolves to and\n\
    where its value came from (a secret is hidden).";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => check(),
        ["--describe"] => {
            print!("{}", environment::describe_text());
            ExitCode::SUCCESS
        }
        ["--describe", "--json"] => {
            println!("{}", environment::describe_json());
            ExitCode::SUCCESS
        }
        ["--effective"] => effective(false),
        ["--effective", "--json"] => effective(true),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn effective(json: bool) -> ExitCode {
    let config = environment::effective(&ProcessEnv);
    let report = config.report();
    if json {
        println!("{}", environment::effective_json(&report));
    } else {
        print!("{}", environment::effective_text(&report));
    }
    let problems = config.check();
    for problem in &problems {
        eprintln!("ERROR: {problem}");
    }
    if problems.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn check() -> ExitCode {
    let problems = environment::check_environment(&ProcessEnv);
    if problems.is_empty() {
        println!("ok: the environment is sound");
        return ExitCode::SUCCESS;
    }
    for problem in &problems {
        eprintln!("ERROR: {problem}");
    }
    ExitCode::from(1)
}
