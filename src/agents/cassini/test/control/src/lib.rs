use clap::Parser;
use harness_common::{HarnessControllerMessage, TestPlan};
use std::process::Stdio;
use tokio::{process::Command, task::AbortHandle};
use ractor::ActorRef;
pub mod service;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Arguments {
    #[arg(short, long)]
    /// Path to a valid .dhall configuration file. It will be evaluated to generate a TestPlan
    pub config: String,
    #[arg(long, default_value = "./target/debug/cassini-server")]
    pub broker_path: String,
    #[arg(long, default_value = "./target/debug/harness-producer")]
    pub producer_path: String,
    #[arg(long, default_value = "./target/debug/harness-sink")]
    pub sink_path: String,
}

pub fn init_logging() {
    let dir = tracing_subscriber::filter::Directive::from(tracing::Level::DEBUG);

    use std::io::stderr;
    use std::io::IsTerminal;
    use tracing_glog::Glog;
    use tracing_glog::GlogFields;
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Registry;

    let fmt = tracing_subscriber::fmt::Layer::default()
        .with_ansi(stderr().is_terminal())
        .with_writer(std::io::stderr)
        .event_format(Glog::default().with_timer(tracing_glog::LocalTime::default()))
        .fmt_fields(GlogFields::default().compact());

    let filter = vec![dir]
        .into_iter()
        .fold(EnvFilter::from_default_env(), |filter, directive| {
            filter.add_directive(directive)
        });

    let subscriber = Registry::default().with(filter).with(fmt);
    tracing::subscriber::set_global_default(subscriber).expect("to set global subscriber");
}

pub fn read_test_config(path: &str) -> TestPlan {
    // let file = File::open(path).expect("Expected to find a test config at {path}");

    let dhall_str =
        std::fs::read_to_string(path).expect("Expected to find dhall configuration at {path}");

    let config: TestPlan = serde_dhall::from_str(&dhall_str).parse().unwrap();

    return config;
}

/// Helper to run an instance of the broker in another thread.
pub async fn spawn_broker(
    control: ActorRef<HarnessControllerMessage>,
    broker_path: String,
) -> AbortHandle {
    return tokio::spawn(async move {
        tracing::info!("Spawning broker...");
        let mut child = Command::new(broker_path)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("failed to spawn broker");

        let status = child.wait().await.expect("broker crashed");
        tracing::warn!(?status, "Broker exited");
        let _ = control.cast(HarnessControllerMessage::Error {
            reason: "Broker crashed".to_string(),
        });
    })
    .abort_handle();
}
