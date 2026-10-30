//! What Cyclops is told when it is deployed: where its configuration file is.
//!
//! The file (YAML, rendered from Dhall and mounted from a ConfigMap) holds the backend, the storage and the
//! environment given to every job. Its secrets are not in it: the object store's keys come in as
//! `CYCLOPS__STORAGE__ACCESS_KEY` and `CYCLOPS__STORAGE__SECRET_KEY` from a Secret, which the loader reads
//! over the file. Which repositories are built is not in it either, but in a signed runtime section
//! (`mappings`).

use polar_env::{Config, ConfigError, Defaulted, Described, Section};

pub const DEFAULT_CONFIG_FILE: &str = "/etc/cyclops/cyclops.yaml";

pub static CONFIG_FILE: Defaulted<String> = Defaulted::text(
    "ORCHESTRATOR_CONFIG_FILE",
    "the path of the orchestrator's configuration file: the backend, the object store and the environment of \
     every job. Its secrets are not in it; see the module documentation",
    DEFAULT_CONFIG_FILE,
);

static VARIABLES: &[&dyn Described] = &[&CONFIG_FILE];

pub static SECTION: Section = Section {
    name: "cyclops",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

pub struct CyclopsSettings {
    pub config_file: String,
}

impl CyclopsSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            config_file: config.read(&CONFIG_FILE)?,
        })
    }
}
