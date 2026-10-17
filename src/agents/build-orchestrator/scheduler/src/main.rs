use analysis_scheduler::{SERVICE_NAME, SchedulerConfig, supervisor::SchedulerSupervisor};
use ractor::Actor;

#[tokio::main]
async fn main() {
    polar::init_logging(SERVICE_NAME.to_string());

    let config = match SchedulerConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("invalid configuration: {e}");
            std::process::exit(2);
        }
    };

    let (_supervisor, handle) = Actor::spawn(
        Some(format!("{SERVICE_NAME}.supervisor")),
        SchedulerSupervisor,
        config,
    )
    .await
    .expect("failed to start analysis scheduler");

    handle.await.expect("analysis scheduler panicked");
}
