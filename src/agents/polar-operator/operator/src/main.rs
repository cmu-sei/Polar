//! polar-operator
//!
//!   polar-operator crd    print the PolarAgent CRD as YAML (`| kubectl apply -f -`)
//!   polar-operator run    reconcile PolarAgents with the cluster in KUBECONFIG
//!
//! With `BROKER_ADDR` set (and the usual Cassini client TLS variables), `run` publishes
//! lifecycle events to the broker; without it, it only keeps the agents alive.

use kube::CustomResourceExt;
use polar_operator::controller::{self, Declared};
use polar_operator::crd::PolarAgent;
use polar_operator::events::Publisher;
use polar_operator::lifecycle::Policy;
use polar_operator::supervisor::{OperatorArgs, OperatorSupervisor};
use ractor::Actor;
use std::process::ExitCode;
use tracing::{error, warn};

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,kube=warn".into()),
        )
        .init();

    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("crd") => match serde_yaml::to_string(&PolarAgent::crd()) {
            Ok(yaml) => {
                print!("{yaml}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                error!("cannot render the CRD: {e}");
                ExitCode::FAILURE
            }
        },
        Some("run") => {
            // kube's TLS client cannot choose between providers on its own.
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
            match run().await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    error!("{e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("usage: polar-operator crd | run");
            ExitCode::from(2)
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let kube = kube::Client::try_default().await?;
    let policy = Policy::default();
    if std::env::var_os("BROKER_ADDR").is_none() {
        warn!("BROKER_ADDR is not set: agents are kept alive, but no lifecycle event is published");
        return controller::run(kube, policy, Publisher::none(), Declared::default()).await;
    }
    let (_, handle) = Actor::spawn(
        Some(polar_operator::OPERATOR_NAME.to_string()),
        OperatorSupervisor,
        OperatorArgs { kube, policy },
    )
    .await?;
    handle.await?;
    // The supervisor only stops when the controller or the broker client has ended.
    anyhow::bail!("the operator stopped")
}
