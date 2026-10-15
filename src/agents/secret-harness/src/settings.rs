//! What the harness reads from its configuration, declared once.

use polar_env::{Config, ConfigError, Defaulted, Described, Env, Required, Section};
use std::time::Duration;

pub static ROLE: Required<String> = Required::text(
    "HARNESS_ROLE",
    "which side of the check this pod plays: `owner` stores a secret and retrieves it, `intruder` \
     (a different identity) tries to take it",
);

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

static VARIABLES: &[&dyn Described] = &[&ROLE, &SECRET_NAME, &PERIOD_SECS, &START_DELAY_SECS];

fn role_is_known(env: &dyn Env) -> Vec<String> {
    match ROLE.read(env) {
        Ok(role) if role.parse::<Role>().is_err() => {
            vec![format!(
                "HARNESS_ROLE `{role}` must be `owner` or `intruder`"
            )]
        }
        _ => Vec::new(),
    }
}

pub static SECTION: Section = Section {
    name: "secret-harness",
    vars: VARIABLES,
    relations: role_is_known,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Owner,
    Intruder,
}

impl std::str::FromStr for Role {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, String> {
        match text {
            "owner" => Ok(Role::Owner),
            "intruder" => Ok(Role::Intruder),
            other => Err(format!("`{other}` is not `owner` or `intruder`")),
        }
    }
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
            // `relations` has already refused a role that is neither.
            role: config.read(&ROLE)?.parse().unwrap_or(Role::Intruder),
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
    fn a_role_is_required_and_must_be_one_of_the_two() {
        assert!(!SECTION.check(&env(&[])).is_empty());
        assert!(SECTION.check(&env(&[("HARNESS_ROLE", "owner")])).is_empty());
        assert!(
            SECTION
                .check(&env(&[("HARNESS_ROLE", "intruder")]))
                .is_empty()
        );
        let problems = SECTION.check(&env(&[("HARNESS_ROLE", "visitor")]));
        assert!(
            problems
                .iter()
                .any(|p| p.contains("visitor") && p.contains("intruder")),
            "{problems:?}"
        );
    }

    #[test]
    fn what_is_not_given_is_defaulted() {
        let vars = env(&[("HARNESS_ROLE", "owner")]);
        let config = Config::builder(&vars).register(&SECTION).build();
        let s = HarnessSettings::read(&config).unwrap();
        assert_eq!(s.role, Role::Owner);
        assert_eq!(s.secret_name, "harness-secret");
        assert_eq!(s.period, Duration::from_secs(60));
        assert_eq!(s.start_delay, Duration::ZERO);
    }

    #[test]
    fn the_intruders_wait_and_the_period_are_read() {
        let vars = env(&[
            ("HARNESS_ROLE", "intruder"),
            ("HARNESS_START_DELAY_SECS", "45"),
            ("HARNESS_PERIOD_SECS", "20"),
            ("HARNESS_SECRET_NAME", "s"),
        ]);
        let config = Config::builder(&vars).register(&SECTION).build();
        let s = HarnessSettings::read(&config).unwrap();
        assert_eq!(s.role, Role::Intruder);
        assert_eq!(s.start_delay, Duration::from_secs(45));
        assert_eq!(s.period, Duration::from_secs(20));
        assert_eq!(s.secret_name, "s");
    }
}
