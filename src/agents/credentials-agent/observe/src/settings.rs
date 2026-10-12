//! What the credentials agent observer reads from its configuration, declared once.

use polar_env::{Config, ConfigError, Defaulted, Described, Section};

pub const DEFAULT_PORT: &str = "8080";

pub static PORT: Defaulted<u16> = Defaulted::number(
    "CREDENTIALS_AGENT_OBSERVER_PORT",
    "the port the observer's HTTP server listens on, on every address",
    DEFAULT_PORT,
);

static VARIABLES: &[&dyn Described] = &[&PORT];

/// The section the credentials agent observer registers.
pub static SECTION: Section = Section {
    name: "credentials-agent-observer",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

pub struct ObserverSettings {
    pub port: u16,
}

impl ObserverSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            port: config.read(&PORT)?,
        })
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
    fn the_port_is_8080_unless_set() {
        assert_eq!(ObserverSettings::read(&config(&[])).unwrap().port, 8080);
        let set = config(&[("CREDENTIALS_AGENT_OBSERVER_PORT", "9090")]);
        assert_eq!(ObserverSettings::read(&set).unwrap().port, 9090);
    }

    #[test]
    fn a_port_that_is_not_one_is_a_problem_before_the_agent_starts() {
        for bad in ["http", "70000", "-1"] {
            let problems = config(&[("CREDENTIALS_AGENT_OBSERVER_PORT", bad)]).check();
            assert_eq!(problems.len(), 1, "{bad}: {problems:?}");
            assert!(
                problems[0].contains("CREDENTIALS_AGENT_OBSERVER_PORT"),
                "{problems:?}"
            );
        }
    }
}
