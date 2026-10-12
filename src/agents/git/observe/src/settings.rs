//! What the git observer reads from its configuration, declared once.
//!
//! `POLAR_GIT_AGENT_CONFIG` and `POLAR_CACHE_ROOT` were read from the process environment where the
//! supervisor started, so an agent given a configuration document could not have them, and nothing
//! could list them or check them before the pod started.

use polar_env::{Config, ConfigError, Defaulted, Described, Section};
use std::path::PathBuf;

pub const DEFAULT_AGENT_CONFIG: &str = "git.yaml";
pub const DEFAULT_CACHE_ROOT: &str = ".polar/cache";

pub static AGENT_CONFIG: Defaulted<String> = Defaulted::text(
    "POLAR_GIT_AGENT_CONFIG",
    "path to the YAML file of per-repository credentials (git.yaml); a file that is not there \
     means no repository is pre-configured, and one that is there and does not parse stops the agent",
    DEFAULT_AGENT_CONFIG,
);

pub static CACHE_ROOT: Defaulted<String> = Defaulted::text(
    "POLAR_CACHE_ROOT",
    "the directory the observed repositories are cloned into; a relative path is relative to the \
     directory the agent was started in. Clones can be fetched again, so it need not survive the pod",
    DEFAULT_CACHE_ROOT,
);

static VARIABLES: &[&dyn Described] = &[&AGENT_CONFIG, &CACHE_ROOT];

/// The section the git observer registers.
pub static SECTION: Section = Section {
    name: "git-repo-observer",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

pub struct ObserverSettings {
    pub agent_config: PathBuf,
    pub cache_root: PathBuf,
}

impl ObserverSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            agent_config: PathBuf::from(config.read(&AGENT_CONFIG)?),
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
    fn nothing_is_required_and_the_defaults_are_the_old_ones() {
        let settings = ObserverSettings::read(&config(&[])).unwrap();
        assert_eq!(settings.agent_config, PathBuf::from("git.yaml"));
        assert_eq!(settings.cache_root, PathBuf::from(".polar/cache"));
        assert!(config(&[]).check().is_empty());
    }

    #[test]
    fn what_is_set_is_used() {
        let settings = ObserverSettings::read(&config(&[
            ("POLAR_GIT_AGENT_CONFIG", "/etc/git/git.yaml"),
            ("POLAR_CACHE_ROOT", "/tmp/polar-cache"),
        ]))
        .unwrap();
        assert_eq!(settings.agent_config, PathBuf::from("/etc/git/git.yaml"));
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
