use serde::{Deserialize, Serialize};

pub mod linker;
pub mod supervisor;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArtifactType {
    ContainerImage,
    Sbom,
}
