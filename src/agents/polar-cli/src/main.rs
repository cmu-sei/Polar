//! polar
//!
//!   polar render <file.dhall>... [-o DIR]       Dhall to YAML
//!   polar plan <plan>...                         read and judge plans; no cluster needed
//!   polar up <plan>... --context C               run plans, in order
//!   polar down <plan>... --context C             take them down, last first (never the CRD)
//!   polar status --context C                     every PolarAgent, and why one is not running
//!
//! The cluster is always named (`--context`, or `POLAR_CONTEXT`): the kubeconfig's current context
//! is never used, so a stale default cannot send a plan somewhere else.

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use polar_cli::exec::{self, Options};
use polar_cli::kube_cluster::KubeCluster;
use polar_cli::plan::Plan;
use polar_cli::{load_plan, random_bytes, render, status};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "polar", version, about = "Administer a Polar deployment")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Evaluate Dhall to YAML (wraps dhall-to-yaml)
    Render {
        files: Vec<PathBuf>,
        /// Write <name>.yaml here instead of printing
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// The file is a list: one YAML document per element
        #[arg(long)]
        documents: bool,
    },
    /// Read and judge plans, and say what running them would do. Needs no cluster
    Plan { plans: Vec<PathBuf> },
    /// Run plans, in order: secrets, objects, then waits, stage by stage
    Up {
        plans: Vec<PathBuf>,
        /// The kube context to use, by name
        #[arg(long, env = "POLAR_CONTEXT")]
        context: String,
        /// Also run this optional stage
        #[arg(long = "with")]
        with: Vec<String>,
        /// Run only this stage
        #[arg(long)]
        only: Vec<String>,
        /// Have the cluster judge each change without making it; wait for nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Take plans down, last stage first. The PolarAgent CRD is never removed
    Down {
        plans: Vec<PathBuf>,
        #[arg(long, env = "POLAR_CONTEXT")]
        context: String,
        /// Also take down the shared stages (the core): other applications may stand on them
        #[arg(long)]
        core: bool,
    },
    /// Every PolarAgent: phase, instance, and why one is not running
    Status {
        #[arg(long, env = "POLAR_CONTEXT")]
        context: String,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    // kube's TLS client cannot choose between providers on its own.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn print(line: String) {
    println!("{line}");
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Render { files, output, documents } => {
            if files.is_empty() {
                bail!("name at least one .dhall file");
            }
            for file in files {
                let yaml = render::dhall_to_yaml(&file, documents)?;
                match &output {
                    Some(dir) => {
                        std::fs::create_dir_all(dir)?;
                        let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
                        let target = dir.join(format!("{stem}.yaml"));
                        std::fs::write(&target, yaml)?;
                        println!("{}", target.display());
                    }
                    None => print!("{yaml}"),
                }
            }
            Ok(())
        }
        Command::Plan { plans } => {
            let mut bad = false;
            for path in plans {
                let plan = load_plan(&path)?;
                let problems = plan.problems();
                if problems.is_empty() {
                    print!("{}", plan.describe());
                } else {
                    bad = true;
                    eprintln!("{} cannot be run:", path.display());
                    for problem in problems {
                        eprintln!("  - {problem}");
                    }
                }
            }
            if bad {
                bail!("a plan has problems");
            }
            Ok(())
        }
        Command::Up { plans, context, with, only, dry_run } => {
            let loaded = load_all(&plans)?;
            check_names(&loaded, &with, &only)?;
            let cluster = KubeCluster::connect(&context).await?;
            println!("using kube context: {context}");
            let options = Options { with, only, dry_run, ..Options::default() };
            exec::up_all(&cluster, &loaded, &options, &mut |n| random_bytes(n), &mut print).await
        }
        Command::Down { plans, context, core } => {
            let mut loaded = load_all(&plans)?;
            let cluster = KubeCluster::connect(&context).await?;
            println!("using kube context: {context}");
            // Last plan first: a plan may stand on the one before it.
            loaded.reverse();
            for plan in &loaded {
                exec::down(&cluster, plan, core, &mut print).await?;
            }
            Ok(())
        }
        Command::Status { context } => {
            let cluster = KubeCluster::connect(&context).await?;
            status::status(&cluster, &mut print).await
        }
    }
}

fn load_all(paths: &[PathBuf]) -> Result<Vec<Plan>> {
    if paths.is_empty() {
        bail!("name at least one plan");
    }
    paths.iter().map(|p: &PathBuf| load_plan(Path::new(p))).collect()
}

/// A stage name that no plan has is a mistake, however many plans there are.
fn check_names(plans: &[Plan], with: &[String], only: &[String]) -> Result<()> {
    for name in with.iter().chain(only) {
        if !plans.iter().any(|p| p.stages.iter().any(|s| &s.name == name)) {
            let have: Vec<_> = plans.iter().flat_map(|p| p.stages.iter().map(|s| s.name.as_str())).collect();
            bail!("no plan has a stage '{name}' (the stages are: {})", have.join(", "));
        }
    }
    Ok(())
}
