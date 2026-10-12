//! Runtime configuration: what an agent works on, in sections it declares itself.
//!
//! An agent's variables say how it runs. A runtime section says what it works on: for the git controller
//! which repositories, under what admission rules; for the observer the credentials it may use. They are
//! signed documents of type `runtime` (docs/architecture/git-controller.md), authored by someone holding
//! `c:update-runtime`, who then cannot change a log level or a shutdown budget. They travel the way a
//! configuration document does and are handled by the same actor; they differ in being replaced whole per
//! section, and in a body the agent itself declares the shape of.

/// Who may change a section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionAuthority {
    /// The ordinary case: a signer holding `c:update-runtime`, with the policy asked first and the certificate
    /// as the fallback (docs/architecture/authority-distribution.md).
    Runtime,
    /// A signer holding this capability and no other will do, and policy is never consulted: the section is
    /// the one policy itself is made of (the rules, the groups), and a rule that governed its own authorship
    /// could lock out every author.
    Capability(cert_issuer_common::capability::Capability),
}

/// A section of an agent's runtime configuration, as the agent declares it.
pub struct RuntimeSection {
    /// Who may change it.
    pub authority: SectionAuthority,
    /// The name a document addresses: unique among the agent's sections.
    pub name: &'static str,
    /// What the section is for, in a sentence.
    pub doc: &'static str,
    /// The body's fields as JSON text, for `--scaffold` and the templates authors are given: each field,
    /// its type, whether it is required, its default and what it is for.
    pub body: &'static str,
    /// Judges a body. Every problem, naming the field: never a value, since a body may hold a secret.
    pub validate: fn(&serde_json::Value) -> Result<(), Vec<String>>,
}

/// Sent on the output port when a section was replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeChanged {
    pub section: String,
    pub version: u64,
}

/// The latest accepted version of a section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeValue {
    pub version: u64,
    pub body: serde_json::Value,
}

/// Every problem with a set of declarations: a name that is empty or not lowercase letters, digits and
/// hyphens, a name used twice, a body description that is not JSON.
pub fn check_declarations(sections: &[&RuntimeSection]) -> Vec<String> {
    let mut problems = Vec::new();
    for (i, section) in sections.iter().enumerate() {
        let valid = !section.name.is_empty()
            && section
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !valid {
            problems.push(format!(
                "runtime section `{}` is not a name (lowercase letters, digits and hyphens)",
                section.name
            ));
        }
        if sections[..i].iter().any(|other| other.name == section.name) {
            problems.push(format!(
                "runtime section `{}` is declared more than once",
                section.name
            ));
        }
        if serde_json::from_str::<serde_json::Value>(section.body).is_err() {
            problems.push(format!(
                "runtime section `{}` describes its body as something that is not JSON",
                section.name
            ));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(name: &'static str, body: &'static str) -> RuntimeSection {
        RuntimeSection {
            authority: SectionAuthority::Runtime,
            name,
            doc: "",
            body,
            validate: |_| Ok(()),
        }
    }

    #[test]
    fn sound_declarations_have_no_problems() {
        let (a, b) = (
            section("repositories", "{}"),
            section("admission-rules", "{}"),
        );
        assert!(check_declarations(&[&a, &b]).is_empty());
    }

    #[test]
    fn a_bad_name_a_repeat_and_a_body_that_is_not_json_are_each_reported_by_name() {
        let (empty, upper, first, second, broken) = (
            section("", "{}"),
            section("Repositories", "{}"),
            section("same", "{}"),
            section("same", "{}"),
            section("ok", "not json"),
        );
        let problems = check_declarations(&[&empty, &upper, &first, &second, &broken]);
        assert_eq!(problems.len(), 4, "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("`Repositories`")));
        assert!(problems.iter().any(|p| p.contains("more than once")));
        assert!(problems.iter().any(|p| p.contains("not JSON")));
    }
}
