use agent_processor::AGENT_PROCESSOR_NAME;
use agent_processor::supervisor::AgentProcessorSupervisor;
use ractor::Actor;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    polar::init_logging(AGENT_PROCESSOR_NAME.to_string());

    let (_, handle) = match Actor::spawn(
        Some(AGENT_PROCESSOR_NAME.to_string()),
        AgentProcessorSupervisor,
        (),
    )
    .await
    {
        Ok(spawned) => spawned,
        Err(e) => {
            tracing::error!("cannot start: {e}");
            return ExitCode::FAILURE;
        }
    };
    let _ = handle.await;
    // The supervisor stops only when its client or graph controller has ended.
    ExitCode::FAILURE
}
