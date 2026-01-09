use std::path::PathBuf;
use tracing_subscriber::{fmt, EnvFilter};

use logging::consume::logs::Durability;
use logging::supervisor::Supervisor;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ----------------------
    // Initialize logging
    // ----------------------
    fmt().with_env_filter(EnvFilter::from_default_env()).init();

    // ----------------------
    // Configuration (could be loaded from file/env)
    // ----------------------
    let registration_id =
        std::env::var("LOG_REGISTRATION_ID").unwrap_or_else(|_| "logging-consumer-1".to_string());
    let db_path =
        PathBuf::from(std::env::var("LOG_DB_PATH").unwrap_or_else(|_| "./rocksdb".to_string()));
    let durability = match std::env::var("LOG_DURABILITY").as_deref() {
        Ok("dev") => Durability::DevLoose,
        Ok("audit") => Durability::AuditStrict,
        _ => Durability::AuditStrict,
    };

    // ----------------------
    // Spawn the logging consumer actor
    // ----------------------
    let _actor = Supervisor::spawn_logging_consumer(registration_id, db_path, durability).await?;

    // ----------------------
    // Block main until shutdown (optional)
    // ----------------------
    tokio::signal::ctrl_c().await?;
    println!("Shutting down logging service...");

    Ok(())
}
