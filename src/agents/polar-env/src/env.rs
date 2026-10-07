//! Where environment values come from.

use std::collections::HashMap;

/// A source of environment values.
///
/// At runtime it is the process environment. In a test, or in a check that judges the
/// environment a workload will be given before it exists, it is a map or a closure, so
/// the same code judges both.
pub trait Env {
    fn var(&self, name: &str) -> Option<String>;
}

/// The environment of this process.
pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn var(&self, name: &str) -> Option<String> {
        // A value that is not valid UTF-8 is still a value; losing a character is better
        // than reporting it as not set.
        std::env::var_os(name).map(|v| v.to_string_lossy().into_owned())
    }
}

impl<F> Env for F
where
    F: Fn(&str) -> Option<String>,
{
    fn var(&self, name: &str) -> Option<String> {
        self(name)
    }
}

impl Env for HashMap<String, String> {
    fn var(&self, name: &str) -> Option<String> {
        HashMap::get(self, name).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn a_closure_is_an_environment() {
        let env = |name: &str| (name == "A").then(|| "1".to_string());
        assert_eq!(env.var("A"), Some("1".to_string()));
        assert_eq!(env.var("B"), None);
    }

    #[test]
    fn a_map_is_an_environment() {
        let env: HashMap<String, String> = [("A".to_string(), "1".to_string())].into();
        assert_eq!(env.var("A"), Some("1".to_string()));
        assert_eq!(env.var("B"), None);
    }

    #[test]
    fn the_process_environment_is_an_environment() {
        // PATH is set in any environment these tests run in.
        assert!(ProcessEnv.var("PATH").is_some());
        assert_eq!(ProcessEnv.var("POLAR_ENV_SURELY_NOT_SET_ANYWHERE"), None);
    }

    #[test]
    fn a_reference_to_an_environment_is_one_too() {
        let env = |_: &str| Some("x".to_string());
        let by_ref: &dyn Env = &env;
        assert_eq!(by_ref.var("A"), Some("x".to_string()));
    }
}
