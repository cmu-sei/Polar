use logger::Durability;
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    polar::init_logging("events.logging.supervisor".to_string());
    let _db_path =
        PathBuf::from(std::env::var("LOG_DB_PATH").unwrap_or_else(|_| "./rocksdb".to_string()));
    let _durability = match std::env::var("LOG_DURABILITY").as_deref() {
        Ok("dev") => Durability::DevLoose,
        Ok("audit") => Durability::AuditStrict,
        _ => Durability::AuditStrict,
    };

    // ----------------------
    // Spawn the logging consumer actor
    // ----------------------
    todo!("Spawn logger");
    // ----------------------
    // Block main until shutdown (optional)
    // ----------------------
}
