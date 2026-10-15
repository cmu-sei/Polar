//! What the git controller reads from its configuration, declared once.

use polar_env::{Config, ConfigError, Defaulted, Described, Section};
use std::time::Duration;

/// The shortest time between two announcements of the set, in seconds. A smaller setting is taken as this.
pub const MIN_REPUBLISH_SECS: u64 = 5;
pub const DEFAULT_REPUBLISH_SECS: &str = "60";

pub static REPUBLISH_SECS: Defaulted<u64> = Defaulted::seconds(
    "POLAR_GIT_CONTROLLER_REPUBLISH_SECS",
    "seconds between announcements of every repository to observe (at least 5; a smaller value is taken as 5). \
     The observer ignores an announcement it has already acted on, and one that starts later hears nothing \
     unless the set is announced again",
    DEFAULT_REPUBLISH_SECS,
)
.dynamic();

static VARIABLES: &[&dyn Described] = &[&REPUBLISH_SECS];

/// The section the git controller registers.
pub static SECTION: Section = Section {
    name: "git-repo-controller",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

/// The time to wait before the next announcement, for a setting of `secs`.
pub fn republish_interval(secs: u64) -> Duration {
    Duration::from_secs(secs.max(MIN_REPUBLISH_SECS))
}

/// The interval in force in `config`.
pub fn read_interval(config: &Config) -> Result<Duration, ConfigError> {
    Ok(republish_interval(config.read(&REPUBLISH_SECS)?))
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
    fn the_section_is_sound_and_its_default_parses() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
        assert!(config(&[]).check().is_empty());
    }

    #[test]
    fn the_default_is_a_minute() {
        assert_eq!(
            read_interval(&config(&[])).unwrap(),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn what_is_set_is_used() {
        let c = config(&[("POLAR_GIT_CONTROLLER_REPUBLISH_SECS", "300")]);
        assert_eq!(read_interval(&c).unwrap(), Duration::from_secs(300));
    }

    #[test]
    fn a_setting_below_the_least_is_taken_as_the_least() {
        for secs in [0, 1, 4] {
            assert_eq!(republish_interval(secs), Duration::from_secs(5), "{secs}");
        }
        assert_eq!(republish_interval(5), Duration::from_secs(5));
        assert_eq!(republish_interval(6), Duration::from_secs(6));
    }

    #[test]
    fn a_value_that_is_not_a_number_is_a_problem_the_check_reports() {
        let c = config(&[("POLAR_GIT_CONTROLLER_REPUBLISH_SECS", "soon")]);
        assert!(!c.check().is_empty());
    }
}
