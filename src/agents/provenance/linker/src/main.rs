use provenance_common::LINKER_SUPERVISOR_NAME;
use provenance_linker::supervisor::ProvenanceSupervisor;
use ractor::Actor;

#[tokio::main]
async fn main() {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    polar::init_logging(LINKER_SUPERVISOR_NAME.to_string());
    let (_, handle) = Actor::spawn(
        Some(LINKER_SUPERVISOR_NAME.to_string()),
        ProvenanceSupervisor,
        (),
    )
    .await
    .expect("Failed to start actor!");

    handle.await.expect("Actor failed to exit cleanly");
}
