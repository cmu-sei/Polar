//! The configuration of one agent: a snapshot of its environment, resolved through the
//! declarations, with runtime overrides on top.
//!
//! This is the pure core of what an agent's configuration actor holds. It has no actors and
//! no I/O of its own, so it can be built, checked and exercised by anything, including a check
//! of the environment a workload will be given before the workload exists.
//!
//! - **Sections are registered.** Whatever reads from the environment (a component, an agent)
//!   registers the [`Section`] that declares what it reads. Nothing outside a registered
//!   section can be read, so the list of variables cannot exist apart from the code that
//!   reads them.
//! - **The environment is read once.** [`ConfigBuilder::build`] copies the declared variables
//!   out of the environment and nothing else, so later changes to the process environment are
//!   not seen, and an undeclared variable (a secret that happens to be set) never becomes
//!   readable or reportable.
//! - **Layers.** A value comes from the first of an override, the environment, or the default,
//!   and [`Config::report`] says which.
//! - **Overrides are transactional.** [`Config::apply_overrides`] refuses a variable that is
//!   not declared or is not [`Dynamic`](crate::Mutability::Dynamic), parses each value with its own
//!   declaration, and re-checks every section on the result. If anything is wrong, nothing
//!   changes. A running agent cannot be put into a state its startup check would refuse.

use crate::{Env, EnvError, Mutability, Readable, Section, VarDesc};
use std::collections::HashMap;
use std::fmt;

/// Where a resolved value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A runtime override.
    Override,
    /// The environment the agent was given.
    Env,
    /// The default in the declaration.
    Default,
    /// Not set anywhere, and not required to be.
    Unset,
}

impl Source {
    /// The name a report shows.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Override => "override",
            Source::Env => "env",
            Source::Default => "default",
            Source::Unset => "unset",
        }
    }
}

/// One variable as it currently resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub section: &'static str,
    pub name: &'static str,
    /// The value as text, `<hidden>` for a secret, or `None` if unset.
    pub value: Option<String>,
    pub source: Source,
    pub mutability: Mutability,
    pub secret: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// Read, but no registered section declares it.
    Undeclared(&'static str),
    Env(EnvError),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Undeclared(name) => write!(
                f,
                "{name} is read, but no registered section declares it; declare it in a section and register that"
            ),
            ConfigError::Env(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ConfigError {}

const HIDDEN: &str = "<hidden>";

/// Overrides on top of the snapshot, as one environment.
struct Layered<'a> {
    overrides: &'a HashMap<&'static str, String>,
    snapshot: &'a HashMap<&'static str, String>,
}

impl Env for Layered<'_> {
    fn var(&self, name: &str) -> Option<String> {
        self.overrides
            .get(name)
            .or_else(|| self.snapshot.get(name))
            .cloned()
    }
}

/// The text a variable resolves to, from the layers, or `None`.
fn effective(
    overrides: &HashMap<&'static str, String>,
    snapshot: &HashMap<&'static str, String>,
    desc: &VarDesc,
) -> (Option<String>, Source) {
    if let Some(value) = overrides.get(desc.name) {
        return (Some(value.clone()), Source::Override);
    }
    match snapshot.get(desc.name).filter(|v| !v.trim().is_empty()) {
        Some(value) => (Some(value.clone()), Source::Env),
        None => match desc.default {
            Some(default) => (Some(default.to_string()), Source::Default),
            None => (None, Source::Unset),
        },
    }
}

/// Registers sections, then takes the snapshot.
pub struct ConfigBuilder<'e> {
    env: &'e dyn Env,
    sections: Vec<&'static Section>,
}

impl<'e> ConfigBuilder<'e> {
    /// A section registered twice counts once.
    pub fn register(mut self, section: &'static Section) -> Self {
        if !self.sections.iter().any(|s| std::ptr::eq(*s, section)) {
            self.sections.push(section);
        }
        self
    }

    /// Copies each declared variable out of the environment, and nothing else.
    pub fn build(self) -> Config {
        let mut snapshot = HashMap::new();
        for section in &self.sections {
            for var in section.vars {
                let name = var.desc().name;
                if let Some(value) = self.env.var(name) {
                    snapshot.insert(name, value);
                }
            }
        }
        Config {
            sections: self.sections,
            snapshot,
            overrides: HashMap::new(),
        }
    }
}

pub struct Config {
    sections: Vec<&'static Section>,
    snapshot: HashMap<&'static str, String>,
    overrides: HashMap<&'static str, String>,
}

/// A configuration is an environment too: what a component's parser reads is the
/// snapshot with overrides on top, and only the variables a registered section declared.
/// This is how a component is handed its settings without ever seeing the process.
impl Env for Config {
    fn var(&self, name: &str) -> Option<String> {
        self.view().var(name)
    }
}

impl Config {
    pub fn builder(env: &dyn Env) -> ConfigBuilder<'_> {
        ConfigBuilder {
            env,
            sections: Vec::new(),
        }
    }

    /// The names the snapshot holds. Only for tests: nothing outside needs to see it, and
    /// that is the point of the test that uses it.
    #[cfg(test)]
    fn captured_names(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.snapshot.keys().copied().collect();
        names.sort();
        names
    }

    fn view(&self) -> Layered<'_> {
        Layered {
            overrides: &self.overrides,
            snapshot: &self.snapshot,
        }
    }

    fn declarations(&self) -> impl Iterator<Item = (&'static str, VarDesc)> + '_ {
        self.sections.iter().flat_map(|section| {
            section
                .vars
                .iter()
                .map(move |var| (section.name, var.desc()))
        })
    }

    fn declared(&self, name: &str) -> Option<VarDesc> {
        self.declarations()
            .map(|(_, desc)| desc)
            .find(|desc| desc.name == name)
    }

    /// Reads a variable through its declaration, if a registered section declares it.
    pub fn read<R: Readable>(&self, var: &R) -> Result<R::Out, ConfigError> {
        if self.declared(var.name()).is_none() {
            return Err(ConfigError::Undeclared(var.name()));
        }
        var.read_from(&self.view()).map_err(ConfigError::Env)
    }

    /// Every problem in the current configuration. All of them, not the first.
    pub fn check(&self) -> Vec<String> {
        let view = self.view();
        self.sections
            .iter()
            .flat_map(|section| section.check(&view))
            .collect()
    }

    /// What is declared, by section.
    pub fn describe(&self) -> Vec<(&'static str, Vec<VarDesc>)> {
        self.sections
            .iter()
            .map(|section| (section.name, section.describe()))
            .collect()
    }

    /// What every declared variable resolves to now, and where that came from.
    pub fn report(&self) -> Vec<Resolved> {
        self.declarations()
            .map(|(section, desc)| {
                let (value, source) = effective(&self.overrides, &self.snapshot, &desc);
                Resolved {
                    section,
                    name: desc.name,
                    value: value.map(|v| if desc.secret { HIDDEN.to_string() } else { v }),
                    source,
                    mutability: desc.mutability,
                    secret: desc.secret,
                }
            })
            .collect()
    }

    /// Applies runtime overrides, all or none.
    ///
    /// A blank value removes that override, returning the variable to the environment or
    /// default. Returns the names whose effective value changed, or every problem found, in
    /// which case nothing has changed. Nothing in a problem repeats a value that was given
    /// for a secret.
    pub fn apply_overrides(
        &mut self,
        changes: &[(&str, &str)],
    ) -> Result<Vec<&'static str>, Vec<String>> {
        let mut problems = Vec::new();
        let mut candidate = self.overrides.clone();

        for (name, value) in changes {
            match self.declared(name) {
                None => problems.push(format!("{name} is not declared by any registered section")),
                Some(desc) if desc.mutability != Mutability::Dynamic => problems.push(format!(
                    "{} cannot be changed at runtime: it is static ({})",
                    desc.name, desc.doc
                )),
                Some(desc) if value.trim().is_empty() => {
                    candidate.remove(desc.name);
                }
                Some(desc) => {
                    candidate.insert(desc.name, (*value).to_string());
                }
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }

        // The result must be one the startup check would accept.
        let view = Layered {
            overrides: &candidate,
            snapshot: &self.snapshot,
        };
        let issues: Vec<String> = self
            .sections
            .iter()
            .flat_map(|section| section.check(&view))
            .collect();
        if !issues.is_empty() {
            return Err(issues);
        }

        let changed = self
            .declarations()
            .map(|(_, desc)| desc)
            .filter(|desc| {
                effective(&self.overrides, &self.snapshot, desc).0
                    != effective(&candidate, &self.snapshot, desc).0
            })
            .map(|desc| desc.name)
            .collect();
        self.overrides = candidate;
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Defaulted, Described, Kind, Mutability, Optional, Required, Section};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // A small component: two static variables and three that may change at runtime, with a
    // relation between the two bounds.
    static HOST: Required<String> = Required::text("HOST", "the host to talk to");
    static NOTE: Optional<String> = Optional::text("NOTE", "a free note");
    static LOW: Defaulted<u64> = Defaulted::number("LOW", "lower bound", "1").dynamic();
    static HIGH: Defaulted<u64> = Defaulted::number("HIGH", "upper bound", "10").dynamic();
    static TOKEN: Optional<String> = Optional::text("TOKEN", "an api token").secret().dynamic();
    static VARS: &[&dyn Described] = &[&HOST, &NOTE, &LOW, &HIGH, &TOKEN];

    fn bounds_in_order(env: &dyn crate::Env) -> Vec<String> {
        let low = LOW.read(env).unwrap();
        let high = HIGH.read(env).unwrap();
        if low >= high {
            vec![format!("LOW ({low}) must be below HIGH ({high})")]
        } else {
            vec![]
        }
    }

    static EXAMPLE: Section = Section {
        name: "example",
        vars: VARS,
        relations: bounds_in_order,
    };

    // A second component, to show sections compose.
    static PORT: Required<u64> = Required::number("PORT", "the port");
    static OTHER_VARS: &[&dyn Described] = &[&PORT];
    static OTHER: Section = Section {
        name: "other",
        vars: OTHER_VARS,
        relations: |_| vec![],
    };

    // A variable no section declares.
    static STRAY: Optional<String> = Optional::text("STRAY", "not declared anywhere");

    fn config(pairs: &[(&str, &str)]) -> Config {
        Config::builder(&env(pairs))
            .register(&EXAMPLE)
            .register(&OTHER)
            .build()
    }

    fn sound() -> Config {
        config(&[("HOST", "h"), ("PORT", "80")])
    }

    // -- reading -----------------------------------------------------------------

    #[test]
    fn a_declared_variable_reads_from_the_environment_or_its_default() {
        let c = config(&[("HOST", "h"), ("PORT", "80"), ("LOW", "3")]);
        assert_eq!(c.read(&HOST).unwrap(), "h");
        assert_eq!(c.read(&PORT).unwrap(), 80);
        assert_eq!(c.read(&LOW).unwrap(), 3);
        assert_eq!(c.read(&HIGH).unwrap(), 10, "the default");
        assert_eq!(c.read(&NOTE).unwrap(), None);
    }

    #[test]
    fn a_variable_no_registered_section_declares_cannot_be_read() {
        // The list of variables cannot exist apart from the code that reads them.
        let c = config(&[("STRAY", "sneaky")]);
        let err = c.read(&STRAY).unwrap_err();
        assert!(matches!(err, ConfigError::Undeclared("STRAY")), "{err:?}");
        assert!(err.to_string().contains("STRAY"), "{err}");
    }

    #[test]
    fn a_malformed_value_reads_as_the_declarations_own_error() {
        let c = config(&[("PORT", "eighty")]);
        let err = c.read(&PORT).unwrap_err().to_string();
        assert!(err.contains("PORT must be a whole number"), "{err}");
        assert!(err.contains("the port"), "{err}");
    }

    // -- the snapshot ------------------------------------------------------------

    #[test]
    fn the_snapshot_does_not_follow_the_environment_afterwards() {
        let live: Rc<RefCell<HashMap<String, String>>> =
            Rc::new(RefCell::new(env(&[("HOST", "before"), ("PORT", "80")])));
        let source = live.clone();
        let c = Config::builder(&move |name: &str| source.borrow().get(name).cloned())
            .register(&EXAMPLE)
            .register(&OTHER)
            .build();
        live.borrow_mut().insert("HOST".into(), "after".into());
        assert_eq!(c.read(&HOST).unwrap(), "before");
    }

    #[test]
    fn only_declared_names_are_captured() {
        // An undeclared secret that happens to be set in the process environment is not
        // copied into the snapshot at all, so it cannot be read, reported, or sit in memory
        // behind a configuration that was never meant to hold it.
        let c = config(&[
            ("AWS_SECRET_ACCESS_KEY", "hunter2"),
            ("HOST", "h"),
            ("PORT", "80"),
            ("LOW", "3"),
        ]);
        assert_eq!(c.captured_names(), ["HOST", "LOW", "PORT"]);
        let everything = format!("{:?}", c.report());
        assert!(!everything.contains("hunter2"), "{everything}");
    }

    #[test]
    fn registering_a_section_twice_counts_once() {
        let c = Config::builder(&env(&[("HOST", "h"), ("PORT", "80")]))
            .register(&EXAMPLE)
            .register(&EXAMPLE)
            .register(&OTHER)
            .build();
        assert_eq!(c.describe().len(), 2);
        assert_eq!(c.check(), Vec::<String>::new());
    }

    // -- checking ----------------------------------------------------------------

    #[test]
    fn a_sound_configuration_has_no_problems() {
        assert_eq!(sound().check(), Vec::<String>::new());
    }

    #[test]
    fn every_registered_section_is_checked_and_every_problem_reported() {
        let c = config(&[("LOW", "x")]);
        let problems = c.check();
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems.iter().any(|p| p.starts_with("HOST is not set")));
        assert!(problems.iter().any(|p| p.starts_with("PORT is not set")));
        assert!(
            problems
                .iter()
                .any(|p| p.starts_with("LOW must be a whole number"))
        );
    }

    #[test]
    fn relations_are_checked_when_the_variables_are_sound() {
        let c = config(&[("HOST", "h"), ("PORT", "80"), ("LOW", "20")]);
        assert_eq!(c.check(), ["LOW (20) must be below HIGH (10)"]);
    }

    // -- describing and reporting ------------------------------------------------

    #[test]
    fn it_describes_what_is_declared_by_section() {
        let d = sound().describe();
        assert_eq!(
            d.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            ["example", "other"]
        );
        let low = d[0].1.iter().find(|v| v.name == "LOW").unwrap();
        assert_eq!(
            (low.kind, low.mutability),
            (Kind::Default, Mutability::Dynamic)
        );
    }

    fn resolved(c: &Config, name: &str) -> Resolved {
        c.report().into_iter().find(|r| r.name == name).unwrap()
    }

    #[test]
    fn the_report_says_where_each_value_came_from() {
        let c = config(&[("HOST", "h"), ("PORT", "80"), ("LOW", "3")]);
        assert_eq!(
            (
                resolved(&c, "HOST").value.as_deref(),
                resolved(&c, "HOST").source
            ),
            (Some("h"), Source::Env)
        );
        assert_eq!(
            (
                resolved(&c, "HIGH").value.as_deref(),
                resolved(&c, "HIGH").source
            ),
            (Some("10"), Source::Default)
        );
        assert_eq!(
            (resolved(&c, "NOTE").value, resolved(&c, "NOTE").source),
            (None, Source::Unset)
        );
    }

    #[test]
    fn a_blank_value_reports_as_unset_not_as_set_to_nothing() {
        let c = config(&[("HOST", "h"), ("PORT", "80"), ("NOTE", "  ")]);
        assert_eq!(resolved(&c, "NOTE").source, Source::Unset);
    }

    #[test]
    fn a_secret_is_hidden_in_the_report() {
        let c = config(&[("HOST", "h"), ("PORT", "80"), ("TOKEN", "s3cret")]);
        let t = resolved(&c, "TOKEN");
        assert_eq!(t.value.as_deref(), Some("<hidden>"));
        assert!(t.secret);
        assert!(!format!("{:?}", c.report()).contains("s3cret"));
    }

    // -- overrides ---------------------------------------------------------------

    #[test]
    fn a_dynamic_variable_can_be_overridden_and_the_report_says_so() {
        let mut c = sound();
        let changed = c.apply_overrides(&[("LOW", "4")]).unwrap();
        assert_eq!(changed, ["LOW"]);
        assert_eq!(c.read(&LOW).unwrap(), 4);
        assert_eq!(resolved(&c, "LOW").source, Source::Override);
    }

    #[test]
    fn an_override_beats_the_environment_which_beats_the_default() {
        let mut c = config(&[("HOST", "h"), ("PORT", "80"), ("LOW", "3")]);
        assert_eq!(c.read(&LOW).unwrap(), 3, "environment over default");
        c.apply_overrides(&[("LOW", "5")]).unwrap();
        assert_eq!(c.read(&LOW).unwrap(), 5, "override over environment");
    }

    #[test]
    fn a_blank_override_removes_it() {
        let mut c = config(&[("HOST", "h"), ("PORT", "80"), ("LOW", "3")]);
        c.apply_overrides(&[("LOW", "5")]).unwrap();
        let changed = c.apply_overrides(&[("LOW", "")]).unwrap();
        assert_eq!(changed, ["LOW"]);
        assert_eq!(c.read(&LOW).unwrap(), 3, "back to the environment");
        assert_eq!(resolved(&c, "LOW").source, Source::Env);
    }

    #[test]
    fn only_what_actually_changed_is_reported_as_changed() {
        let mut c = sound();
        assert_eq!(c.apply_overrides(&[("LOW", "4")]).unwrap(), ["LOW"]);
        assert_eq!(
            c.apply_overrides(&[("LOW", "4")]).unwrap(),
            Vec::<&str>::new(),
            "the same value again"
        );
        // Overriding with what the default already is changes nothing either.
        assert_eq!(
            c.apply_overrides(&[("HIGH", "10")]).unwrap(),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn a_static_variable_cannot_be_overridden() {
        // The code decides what may change while the agent runs, not the repository.
        let mut c = sound();
        let problems = c.apply_overrides(&[("HOST", "elsewhere")]).unwrap_err();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("HOST"), "{problems:?}");
        assert!(problems[0].contains("static"), "{problems:?}");
        assert_eq!(c.read(&HOST).unwrap(), "h", "unchanged");
    }

    #[test]
    fn an_undeclared_variable_cannot_be_overridden() {
        let mut c = sound();
        let problems = c.apply_overrides(&[("NOT_A_THING", "1")]).unwrap_err();
        assert!(problems[0].contains("NOT_A_THING"), "{problems:?}");
        assert!(problems[0].contains("not declared"), "{problems:?}");
    }

    #[test]
    fn a_bad_override_is_refused_naming_the_variable_and_what_it_is_for() {
        let mut c = sound();
        let problems = c.apply_overrides(&[("LOW", "lots")]).unwrap_err();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("LOW must be a whole number"),
            "{problems:?}"
        );
        assert!(problems[0].contains("lower bound"), "{problems:?}");
        assert_eq!(c.read(&LOW).unwrap(), 1, "the old value stands");
    }

    #[test]
    fn an_override_that_breaks_a_relation_is_refused() {
        // Each value is fine alone; together they are a state the startup check would refuse.
        let mut c = sound();
        let problems = c.apply_overrides(&[("LOW", "50")]).unwrap_err();
        assert_eq!(problems, ["LOW (50) must be below HIGH (10)"]);
        assert_eq!(c.read(&LOW).unwrap(), 1);
    }

    #[test]
    fn a_set_of_overrides_that_is_sound_together_is_applied_together() {
        // LOW alone would break the relation; with HIGH raised alongside it, it does not.
        let mut c = sound();
        let mut changed = c
            .apply_overrides(&[("LOW", "50"), ("HIGH", "100")])
            .unwrap();
        changed.sort();
        assert_eq!(changed, ["HIGH", "LOW"]);
        assert_eq!((c.read(&LOW).unwrap(), c.read(&HIGH).unwrap()), (50, 100));
    }

    #[test]
    fn one_bad_change_among_good_ones_applies_none() {
        let mut c = sound();
        let problems = c
            .apply_overrides(&[("LOW", "4"), ("HOST", "elsewhere"), ("HIGH", "20")])
            .unwrap_err();
        assert!(!problems.is_empty());
        assert_eq!((c.read(&LOW).unwrap(), c.read(&HIGH).unwrap()), (1, 10));
    }

    #[test]
    fn a_secret_override_is_applied_and_still_hidden() {
        let mut c = sound();
        c.apply_overrides(&[("TOKEN", "fresh-secret")]).unwrap();
        assert_eq!(c.read(&TOKEN).unwrap().as_deref(), Some("fresh-secret"));
        let r = resolved(&c, "TOKEN");
        assert_eq!(
            (r.value.as_deref(), r.source),
            (Some("<hidden>"), Source::Override)
        );
        assert!(!format!("{:?}", c.report()).contains("fresh-secret"));
    }

    #[test]
    fn a_refused_override_of_a_secret_does_not_echo_it() {
        let mut c = sound();
        // TOKEN is text, so any value parses; make the refusal come from another change.
        let problems = c
            .apply_overrides(&[("TOKEN", "leak-me"), ("HOST", "x")])
            .unwrap_err();
        assert!(!problems.join(" ").contains("leak-me"), "{problems:?}");
    }

    // -- a Config is itself an environment --------------------------------------------

    #[test]
    fn a_config_read_as_an_env_shows_declared_variables_and_nothing_else() {
        let c = Config::builder(&env(&[("HOST", "h"), ("STRAY", "ambient")]))
            .register(&EXAMPLE)
            .build();
        let as_env: &dyn crate::Env = &c;
        assert_eq!(as_env.var("HOST").as_deref(), Some("h"));
        // Set in the source environment, declared by nothing: it never got in.
        assert_eq!(as_env.var("STRAY"), None);
    }

    #[test]
    fn a_config_read_as_an_env_shows_overrides_over_the_snapshot() {
        let mut c = Config::builder(&env(&[("HOST", "h"), ("LOW", "2")]))
            .register(&EXAMPLE)
            .build();
        c.apply_overrides(&[("LOW", "4")]).unwrap();
        let as_env: &dyn crate::Env = &c;
        assert_eq!(as_env.var("LOW").as_deref(), Some("4"));
    }

    #[test]
    fn components_can_read_their_own_settings_from_a_config() {
        // The shape the health actor uses: a component's parser takes an Env, and is
        // handed the Config the builder took.
        let c = Config::builder(&env(&[("HOST", "h"), ("HIGH", "30")]))
            .register(&EXAMPLE)
            .build();
        assert_eq!(HIGH.read(&c).unwrap(), 30);
    }

    #[test]
    fn a_source_has_a_name_for_reports() {
        assert_eq!(Source::Override.as_str(), "override");
        assert_eq!(Source::Env.as_str(), "env");
        assert_eq!(Source::Default.as_str(), "default");
        assert_eq!(Source::Unset.as_str(), "unset");
    }
}
