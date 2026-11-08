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

//! What the harness reads from its configuration, declared once.

use polar_env::{Config, ConfigError, Defaulted, Described, Section};
use std::time::Duration;

pub static SECRET_NAME: Defaulted<String> = Defaulted::text(
    "HARNESS_SECRET_NAME",
    "the name of the secret both pods use, under the owner's identity (`<identity>/<name>`): an identity may store only under its own name, so the owner stores it and the intruder is refused",
    "harness-secret",
);

pub static PERIOD_SECS: Defaulted<u64> = Defaulted::number(
    "HARNESS_PERIOD_SECS",
    "seconds between rounds of checks; the owner's later rounds show that nothing the intruder did \
     changed its secret",
    "60",
);

pub static START_DELAY_SECS: Defaulted<u64> = Defaulted::number(
    "HARNESS_START_DELAY_SECS",
    "seconds to wait before the first round. The intruder waits for the owner to have stored, so \
     that its attempt to store the same name finds it taken and not free",
    "0",
);

static VARIABLES: &[&dyn Described] = &[&SECRET_NAME, &PERIOD_SECS, &START_DELAY_SECS];

pub static SECTION: Section = Section {
    name: "secret-harness",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

/// Which side of the check this build plays: `owner` stores a secret and retrieves it, `intruder` (a different
/// identity) tries to take it. Fixed by the build (the `intruder` feature), as the workload's name is.
#[cfg(not(feature = "intruder"))]
pub const ROLE: Role = Role::Owner;
#[cfg(feature = "intruder")]
pub const ROLE: Role = Role::Intruder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Owner,
    Intruder,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Intruder => "intruder",
        }
    }
}

pub struct HarnessSettings {
    pub role: Role,
    pub secret_name: String,
    pub period: Duration,
    pub start_delay: Duration,
}

impl HarnessSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            role: ROLE,
            secret_name: config.read(&SECRET_NAME)?,
            period: Duration::from_secs(config.read(&PERIOD_SECS)?),
            start_delay: Duration::from_secs(config.read(&START_DELAY_SECS)?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_section_is_sound_and_its_defaults_parse() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
    }

    #[test]
    fn the_role_and_the_name_are_the_builds() {
        // One binary per workload: the name is the role's workload, whichever build this is.
        assert_eq!(
            crate::supervisor::SERVICE_NAME,
            format!("secret-harness-{}", ROLE.name())
        );
    }

    #[test]
    fn what_is_not_given_is_defaulted() {
        let vars = env(&[]);
        let config = Config::builder(&vars).register(&SECTION).build();
        let s = HarnessSettings::read(&config).unwrap();
        assert_eq!(s.role, ROLE);
        assert_eq!(s.secret_name, "harness-secret");
        assert_eq!(s.period, Duration::from_secs(60));
        assert_eq!(s.start_delay, Duration::ZERO);
    }

    #[test]
    fn the_intruders_wait_and_the_period_are_read() {
        let vars = env(&[
            ("HARNESS_START_DELAY_SECS", "45"),
            ("HARNESS_PERIOD_SECS", "20"),
            ("HARNESS_SECRET_NAME", "s"),
        ]);
        let config = Config::builder(&vars).register(&SECTION).build();
        let s = HarnessSettings::read(&config).unwrap();
        assert_eq!(s.role, ROLE);
        assert_eq!(s.start_delay, Duration::from_secs(45));
        assert_eq!(s.period, Duration::from_secs(20));
        assert_eq!(s.secret_name, "s");
    }
}
