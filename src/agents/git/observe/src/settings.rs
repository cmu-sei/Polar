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

//! What the git observer reads from its configuration, declared once.
//!
//! `POLAR_CACHE_ROOT` was read from the process environment where the supervisor started, so an agent
//! given a configuration document could not have it, and nothing could list it or check it before the
//! pod started. (`POLAR_GIT_AGENT_CONFIG`, the path of a `git.yaml` of credentials, is gone: credentials
//! reach the observer in its `credentials` runtime section.)

use polar_env::{Config, ConfigError, Defaulted, Described, Section};
use std::path::PathBuf;

pub const DEFAULT_CACHE_ROOT: &str = ".polar/cache";

pub static CACHE_ROOT: Defaulted<String> = Defaulted::text(
    "POLAR_CACHE_ROOT",
    "the directory the observed repositories are cloned into; a relative path is relative to the \
     directory the agent was started in. Clones can be fetched again, so it need not survive the pod",
    DEFAULT_CACHE_ROOT,
);

static VARIABLES: &[&dyn Described] = &[&CACHE_ROOT];

/// The section the git observer registers.
pub static SECTION: Section = Section {
    name: "git-repo-observer",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

pub struct ObserverSettings {
    pub cache_root: PathBuf,
}

impl ObserverSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            cache_root: PathBuf::from(config.read(&CACHE_ROOT)?),
        })
    }

    /// The cache directory as an absolute path: a relative one is taken from where the agent runs.
    pub fn absolute_cache_root(&self) -> std::io::Result<PathBuf> {
        if self.cache_root.is_absolute() {
            Ok(self.cache_root.clone())
        } else {
            Ok(std::env::current_dir()?.join(&self.cache_root))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(pairs: &[(&str, &str)]) -> Config {
        let env: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::builder(&env).register(&SECTION).build()
    }

    #[test]
    fn the_section_is_sound_and_its_defaults_parse() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
    }

    #[test]
    fn nothing_is_required_and_the_default_is_the_old_one() {
        let settings = ObserverSettings::read(&config(&[])).unwrap();
        assert_eq!(settings.cache_root, PathBuf::from(".polar/cache"));
        assert!(config(&[]).check().is_empty());
    }

    #[test]
    fn what_is_set_is_used() {
        let settings =
            ObserverSettings::read(&config(&[("POLAR_CACHE_ROOT", "/tmp/polar-cache")])).unwrap();
        assert_eq!(
            settings.absolute_cache_root().unwrap(),
            PathBuf::from("/tmp/polar-cache")
        );
    }

    #[test]
    fn a_relative_cache_root_is_taken_from_where_the_agent_runs() {
        let settings = ObserverSettings::read(&config(&[])).unwrap();
        let absolute = settings.absolute_cache_root().unwrap();
        assert!(absolute.is_absolute());
        assert!(absolute.ends_with(".polar/cache"));
    }
}
