//! Reading settings from the environment, the same way everywhere in the crate.
//!
//! The health actor and the liveness probe read the same variables from the same
//! pod environment, so they must agree on what a malformed value means. They share
//! this, rather than each keeping a copy that could drift.

/// Reads a whole number. `None` when the variable is unset or blank; an error
/// naming the variable when it is set to something that is not a number.
///
/// `lookup` is how the environment is read, so a test can supply values without
/// touching the process environment.
pub(crate) fn number<T: std::str::FromStr>(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &str,
) -> Result<Option<T>, String> {
    match lookup(name) {
        None => Ok(None),
        Some(raw) if raw.trim().is_empty() => Ok(None),
        Some(raw) => raw
            .trim()
            .parse::<T>()
            .map(Some)
            .map_err(|_| format!("{name} must be a whole number of seconds, got '{raw}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(value: Option<&'static str>) -> impl Fn(&str) -> Option<String> {
        move |_| value.map(str::to_string)
    }

    #[test]
    fn unset_and_blank_mean_nothing_was_said() {
        assert_eq!(number::<u64>(&lookup(None), "X"), Ok(None));
        assert_eq!(number::<u64>(&lookup(Some("")), "X"), Ok(None));
        assert_eq!(number::<u64>(&lookup(Some("  ")), "X"), Ok(None));
    }

    #[test]
    fn a_number_is_read_with_its_whitespace_trimmed() {
        assert_eq!(number::<u64>(&lookup(Some(" 42 ")), "X"), Ok(Some(42)));
        assert_eq!(number::<i64>(&lookup(Some("-3")), "X"), Ok(Some(-3)));
    }

    #[test]
    fn anything_else_is_an_error_that_names_the_variable_and_the_value() {
        for bad in ["soon", "1.5", "30s", "0x10"] {
            let err = number::<u64>(&lookup(Some(bad)), "POLAR_HEALTH_TICK_SECS").unwrap_err();
            assert!(err.contains("POLAR_HEALTH_TICK_SECS"), "{err}");
            assert!(err.contains(bad), "{err}");
        }
    }

    #[test]
    fn a_negative_is_not_a_number_for_an_unsigned_type() {
        assert!(number::<u64>(&lookup(Some("-3")), "X").is_err());
    }
}
