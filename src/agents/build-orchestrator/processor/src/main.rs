#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    polar::init_logging("polar.builds.processor".to_string());

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let (_, handle) = ractor::Actor::spawn(
        Some("polar.builds.processor.supervisor".to_string()),
        build_processor::BuildProcessorSupervisor,
        (),
    )
    .await
    .expect("Expected to start observer agent");
    let _ = handle.await;

    Ok(())
}
