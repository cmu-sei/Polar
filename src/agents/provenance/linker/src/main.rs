use provenance_common::LINKER_SUPERVISOR_NAME;
use provenance_linker::supervisor::ProvenanceSupervisor;
use ractor::Actor;

#[tokio::main]
async fn main() {
    polar::init_logging();
    let (_, handle) = Actor::spawn(
        Some(LINKER_SUPERVISOR_NAME.to_string()),
        ProvenanceSupervisor,
        (),
    )
    .await
    .expect("Failed to start actor!");

    handle.await.expect("Actor failed to exit cleanly");
}
