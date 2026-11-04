/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

//! What build-orchestrator is told when it is deployed: where its configuration file is.
//!
//! The file (YAML, rendered from Dhall and mounted from a ConfigMap) holds the backend, the storage and the
//! environment given to every job. Its secrets are not in it: the object store's keys come in as
//! `BUILD_ORCHESTRATOR__STORAGE__ACCESS_KEY` and `BUILD_ORCHESTRATOR__STORAGE__SECRET_KEY` from a Secret, which the loader reads
//! over the file. Which repositories are built is not in it either, but in a signed runtime section
//! (`mappings`).

use polar_env::{Config, ConfigError, Defaulted, Described, Section};

pub const DEFAULT_CONFIG_FILE: &str = "/etc/build-orchestrator/build-orchestrator.yaml";

pub static CONFIG_FILE: Defaulted<String> = Defaulted::text(
    "ORCHESTRATOR_CONFIG_FILE",
    "the path of the orchestrator's configuration file: the backend, the object store and the environment of \
     every job. Its secrets are not in it; see the module documentation",
    DEFAULT_CONFIG_FILE,
);

static VARIABLES: &[&dyn Described] = &[&CONFIG_FILE];

pub static SECTION: Section = Section {
    name: "build-orchestrator",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

pub struct BuildOrchestratorSettings {
    pub config_file: String,
}

impl BuildOrchestratorSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            config_file: config.read(&CONFIG_FILE)?,
        })
    }
}
