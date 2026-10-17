//! Podman build backend for the Cyclops orchestrator.
//!
//! Implements [`orchestrator_core::backend::BuildBackend`] using the local
//! Podman daemon over its Docker-compatible Unix socket API.
//!
//! ## Why Podman
//!
//! The Kubernetes backend requires a reachable API server for every backend
//! operation. Whenever the API server is centrally
//! hosted and becomes unreachable — taking down job
//! submission with it. Podman communicates over a local Unix socket with no
//! network dependency, making it partition-tolerant by construction.
//!
//! ## Crate Layout
//!
//! - [`backend`]: [`PodmanBackend`] — the public entry point; implements the trait.
//! - [`container`]: Container lifecycle (create, start, inspect, stop, remove, logs).
//! - [`image`]: Image presence check and pull with configurable pull policy.
//! - [`error`]: [`PodmanError`] — internal error type that maps exhaustively to
//!   [`orchestrator_core::error::BackendError`] at the trait boundary.
//!
//! ## Usage
//!
//! ```rust,no_run
//! use orchestrator_backend_podman::backend::{PodmanBackend, PodmanConfig};
//! use orchestrator_backend_podman::image::ImagePullPolicy;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let config = PodmanConfig {
//!         socket_path: "/run/user/1000/podman/podman.sock".into(),
//!         image_pull_policy: ImagePullPolicy::IfNotPresent,
//!     };
//!
//!     let backend = PodmanBackend::connect(config)?;
//!     backend.ping().await?;
//!
//!     // backend is now ready to be wrapped in Arc<dyn BuildBackend>
//!     // and handed to OrchestratorSupervisor.
//!     Ok(())
//! }
//! ```

pub mod backend;
pub mod container;
pub mod error;
pub mod image;
pub mod pod;
