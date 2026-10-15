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

/// The shortest time a proposal is kept without its proposer announcing it again, in seconds. A smaller setting
/// is taken as this.
pub const MIN_PROPOSAL_TTL_SECS: u64 = 300;
pub const DEFAULT_PROPOSAL_TTL_SECS: &str = "86400";

pub static PROPOSAL_TTL_SECS: Defaulted<u64> = Defaulted::seconds(
    "POLAR_GIT_CONTROLLER_PROPOSAL_TTL_SECS",
    "seconds a proposed repository is kept without its proposer announcing it again (at least 300; a smaller \
     value is taken as 300). An entry a person wants kept for good belongs in the signed repositories section",
    DEFAULT_PROPOSAL_TTL_SECS,
)
.dynamic();

pub const DEFAULT_WARMUP_SECS: &str = "600";

pub static WARMUP_SECS: Defaulted<u64> = Defaulted::seconds(
    "POLAR_GIT_CONTROLLER_WARMUP_SECS",
    "seconds after the controller starts during which the set it announces is marked incomplete, so that the \
     observer stops nothing: the signed sections may not have reached it yet and the proposers have not \
     announced again. Make it longer than the longest time between a proposer's announcements. 0 means no wait",
    DEFAULT_WARMUP_SECS,
)
.dynamic();

static VARIABLES: &[&dyn Described] = &[&REPUBLISH_SECS, &PROPOSAL_TTL_SECS, &WARMUP_SECS];

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

/// How long a proposal is kept without being announced again, for a setting of `secs`.
pub fn proposal_ttl(secs: u64) -> Duration {
    Duration::from_secs(secs.max(MIN_PROPOSAL_TTL_SECS))
}

/// The proposal time limit in force in `config`.
pub fn read_proposal_ttl(config: &Config) -> Result<Duration, ConfigError> {
    Ok(proposal_ttl(config.read(&PROPOSAL_TTL_SECS)?))
}

/// The warm-up in force in `config`.
pub fn read_warmup(config: &Config) -> Result<Duration, ConfigError> {
    Ok(Duration::from_secs(config.read(&WARMUP_SECS)?))
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

    #[test]
    fn a_proposal_is_kept_a_day_by_default_and_never_less_than_the_least() {
        assert_eq!(
            read_proposal_ttl(&config(&[])).unwrap(),
            Duration::from_secs(86_400)
        );
        let c = config(&[("POLAR_GIT_CONTROLLER_PROPOSAL_TTL_SECS", "7200")]);
        assert_eq!(read_proposal_ttl(&c).unwrap(), Duration::from_secs(7200));
        for secs in [0, 1, 299] {
            assert_eq!(proposal_ttl(secs), Duration::from_secs(300), "{secs}");
        }
        assert_eq!(proposal_ttl(300), Duration::from_secs(300));
    }

    #[test]
    fn the_warm_up_is_ten_minutes_by_default_and_may_be_none() {
        assert_eq!(read_warmup(&config(&[])).unwrap(), Duration::from_secs(600));
        let c = config(&[("POLAR_GIT_CONTROLLER_WARMUP_SECS", "0")]);
        assert_eq!(read_warmup(&c).unwrap(), Duration::ZERO);
    }

    #[test]
    fn the_new_settings_are_dynamic_so_a_signed_document_may_change_them() {
        use polar_env::Mutability;
        assert_eq!(PROPOSAL_TTL_SECS.desc().mutability, Mutability::Dynamic);
        assert_eq!(WARMUP_SECS.desc().mutability, Mutability::Dynamic);
    }
}
