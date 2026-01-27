// pub mod logger;

pub struct EventLoggingSupervisor;

/// --- Durability profile ---
#[derive(Clone, Copy, Debug)]
pub enum Durability {
    DevLoose,    // fast: do not fsync every commit
    AuditStrict, // safe: fsync (WAL) before returning
}
