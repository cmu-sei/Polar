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

//! What can be wrong with a variable, said so that someone can act on it.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// Not set, or set to nothing.
    Missing,
    /// Set, but not to something of the right kind.
    Malformed {
        /// What was given, or `<hidden>` for a secret.
        shown: String,
        /// What it should have been, for example "must be a whole number of seconds".
        reason: String,
    },
}

/// A variable that is missing or malformed. It names the variable and says what it is
/// for, which `std::env::var(..)?` does not: that says only "environment variable not
/// found".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvError {
    pub name: &'static str,
    pub doc: &'static str,
    pub problem: Problem,
}

impl fmt::Display for EnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.problem {
            Problem::Missing => write!(f, "{} is not set ({})", self.name, self.doc),
            Problem::Malformed { shown, reason } => {
                write!(f, "{} {reason}, got '{shown}' ({})", self.name, self.doc)
            }
        }
    }
}

impl std::error::Error for EnvError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_variable_is_named_with_what_it_is_for() {
        let e = EnvError {
            name: "GRAPH_ENDPOINT",
            doc: "Bolt address of the Neo4j server",
            problem: Problem::Missing,
        };
        assert_eq!(
            e.to_string(),
            "GRAPH_ENDPOINT is not set (Bolt address of the Neo4j server)"
        );
    }

    #[test]
    fn a_malformed_variable_shows_what_was_given() {
        let e = EnvError {
            name: "POLAR_HEALTH_TICK_SECS",
            doc: "seconds between ticks",
            problem: Problem::Malformed {
                shown: "soon".into(),
                reason: "must be a whole number of seconds".into(),
            },
        };
        assert_eq!(
            e.to_string(),
            "POLAR_HEALTH_TICK_SECS must be a whole number of seconds, got 'soon' (seconds between ticks)"
        );
    }

    #[test]
    fn it_is_an_error() {
        fn takes_error(_: &dyn std::error::Error) {}
        takes_error(&EnvError {
            name: "X",
            doc: "d",
            problem: Problem::Missing,
        });
    }
}
