use build_processor::supervisor::BuildProcessorSupervisor;
use polar::BUILD_PROCESSOR_NAME;
use ractor::Actor;

#[tokio::main]
async fn main() {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    polar::init_logging(BUILD_PROCESSOR_NAME.to_string());
    let (_, handle) = Actor::spawn(
        Some(BUILD_PROCESSOR_NAME.to_string()),
        BuildProcessorSupervisor,
        (),
    )
    .await
    .expect("Failed to start actor!");

    handle.await.expect("Actor failed to exit cleanly");
}
