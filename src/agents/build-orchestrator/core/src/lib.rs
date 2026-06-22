pub mod backend;
pub mod error;
pub mod events;
pub mod types;

/// The path inside every job where source code is available.
/// In the kubernetes backends both the init container (writer) and the main container (reader) mount
/// the workspace emptyDir volume here. This is a framework convention.
pub const WORKSPACE_PATH: &str = "/workspace";

pub const TLS_CLIENT_KEY: &str = "tls.key";
pub const TLS_CLIENT_CERT: &str = "tls.crt";
pub const TLS_CA_CERT: &str = "ca.crt";
pub const TLS_CERT_DIR: &str = "/etc/tls";

pub fn tls_client_cert_path() -> String {
    format!("{TLS_CERT_DIR}/{TLS_CLIENT_CERT}")
}

pub fn tls_client_key_path() -> String {
    format!("{TLS_CERT_DIR}/{TLS_CLIENT_KEY}")
}

pub fn tls_ca_cert_path() -> String {
    format!("{TLS_CERT_DIR}/{TLS_CA_CERT}")
}
