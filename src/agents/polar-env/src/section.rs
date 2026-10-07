//! A component's variables, with the relations between them.

use crate::{Described, Env, Problem, VarDesc};

/// What one component reads from the environment.
///
/// `vars` is every variable it reads. `relations` is what must hold between them, such as
/// "the room before expiry covers two ticks"; it is only run when every variable parses,
/// so it can read them without repeating the errors already reported.
pub struct Section {
    pub name: &'static str,
    pub vars: &'static [&'static dyn Described],
    pub relations: fn(&dyn Env) -> Vec<String>,
}

impl Section {
    /// Every problem with `env`, said so that someone can act on it. All of them, not
    /// the first.
    pub fn check(&self, env: &dyn Env) -> Vec<String> {
        let mut problems: Vec<String> = self
            .vars
            .iter()
            .filter_map(|var| var.validate(env).err())
            .map(|error| error.to_string())
            .collect();
        if problems.is_empty() {
            problems.extend((self.relations)(env));
        }
        problems
    }

    pub fn describe(&self) -> Vec<VarDesc> {
        self.vars.iter().map(|var| var.desc()).collect()
    }

    /// A default that does not parse. A component's own tests call this, since nothing
    /// else would notice until the default was used.
    pub fn defaults_problems(&self) -> Vec<String> {
        let nothing = |_: &str| None::<String>;
        self.vars
            .iter()
            .filter_map(|var| var.validate(&nothing).err())
            .filter(|error| matches!(error.problem, Problem::Malformed { .. }))
            .map(|error| error.to_string())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Defaulted, Described, Kind, Optional, Required};
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    static HOST: Required<String> = Required::text("HOST", "the host");
    static PORT: Required<u64> = Required::number("PORT", "the port");
    static LOW: Defaulted<u64> = Defaulted::number("LOW", "lower bound", "1");
    static HIGH: Defaulted<u64> = Defaulted::number("HIGH", "upper bound", "10");
    static NOTE: Optional<String> = Optional::text("NOTE", "a note");
    static VARS: &[&dyn Described] = &[&HOST, &PORT, &LOW, &HIGH, &NOTE];

    fn bounds_in_order(env: &dyn crate::Env) -> Vec<String> {
        let low = LOW.read(env).unwrap();
        let high = HIGH.read(env).unwrap();
        if low >= high {
            vec![format!("LOW ({low}) must be below HIGH ({high})")]
        } else {
            vec![]
        }
    }

    static SECTION: Section = Section {
        name: "example",
        vars: VARS,
        relations: bounds_in_order,
    };

    #[test]
    fn a_complete_environment_has_no_problems() {
        assert_eq!(
            SECTION.check(&env(&[("HOST", "h"), ("PORT", "80")])),
            Vec::<String>::new()
        );
    }

    #[test]
    fn every_problem_is_reported_not_just_the_first() {
        let problems = SECTION.check(&env(&[("PORT", "eighty"), ("LOW", "x")]));
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems.iter().any(|p| p.starts_with("HOST is not set")));
        assert!(
            problems
                .iter()
                .any(|p| p.starts_with("PORT must be a whole number"))
        );
        assert!(
            problems
                .iter()
                .any(|p| p.starts_with("LOW must be a whole number"))
        );
    }

    #[test]
    fn relations_are_checked_when_the_variables_are_sound() {
        let problems = SECTION.check(&env(&[("HOST", "h"), ("PORT", "80"), ("LOW", "20")]));
        assert_eq!(problems, ["LOW (20) must be below HIGH (10)"]);
    }

    #[test]
    fn relations_are_not_run_on_variables_that_do_not_parse() {
        // The relation reads LOW and HIGH; a malformed LOW is already reported, once.
        let problems = SECTION.check(&env(&[("HOST", "h"), ("PORT", "80"), ("LOW", "x")]));
        assert_eq!(problems.len(), 1, "{problems:?}");
    }

    #[test]
    fn a_section_describes_its_variables() {
        let d = SECTION.describe();
        assert_eq!(
            d.iter().map(|v| v.name).collect::<Vec<_>>(),
            ["HOST", "PORT", "LOW", "HIGH", "NOTE"]
        );
        assert_eq!(d[0].kind, Kind::Required);
        assert_eq!(d[2].default, Some("1"));
    }

    #[test]
    fn defaults_that_do_not_parse_are_found_without_any_environment() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());

        static BAD: Defaulted<u64> = Defaulted::number("BAD", "d", "ten");
        static BAD_VARS: &[&dyn Described] = &[&HOST, &BAD];
        static BAD_SECTION: Section = Section {
            name: "bad",
            vars: BAD_VARS,
            relations: |_| vec![],
        };
        let problems = BAD_SECTION.defaults_problems();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("BAD"), "{problems:?}");
    }
}
