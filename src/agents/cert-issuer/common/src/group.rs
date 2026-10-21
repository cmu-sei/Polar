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

//! The groups the catalog puts a workload in, carried in its certificate as `g:<group>`, beside the `c:` capabilities
//! and the `p:` permissions (docs/architecture/policy-enforcement.md, "Decisions"). They are the floor of a
//! principal's group membership: the signed groups document adds to or overrides them at run time, as Cedar does the
//! certificate. Which groups there are, and what being in one implies, is not decided here.

use serde::{Deserialize, Serialize};

/// A group a certificate says its holder is in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Group(pub String);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GroupParseError {
    #[error("missing 'g:' prefix")]
    MissingPrefix,
    #[error(
        "{0:?} is not a group name: lower-case letters, digits and '-', starting with a letter or digit"
    )]
    InvalidName(String),
}

/// Whether `name` is a group name: lower-case letters, digits and `-`, starting with a letter or digit, at most 63.
pub fn is_group_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

impl Group {
    pub fn to_san_string(&self) -> String {
        format!("g:{}", self.0)
    }

    /// The inverse of [`to_san_string`](Self::to_san_string).
    pub fn from_san_string(s: &str) -> Result<Self, GroupParseError> {
        let name = s.strip_prefix("g:").ok_or(GroupParseError::MissingPrefix)?;
        if !is_group_name(name) {
            return Err(GroupParseError::InvalidName(name.to_string()));
        }
        Ok(Self(name.to_string()))
    }
}

/// Every group a certificate carries; anything else in its names is skipped.
pub fn extract_groups(
    certificate_pem: &str,
) -> Result<Vec<Group>, crate::permission::PermissionExtractionError> {
    Ok(crate::permission::san_strings(certificate_pem)?
        .iter()
        .filter_map(|s| Group::from_san_string(s).ok())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_group_is_written_and_read_in_the_documented_form() {
        assert_eq!(Group("encryption".into()).to_san_string(), "g:encryption");
        assert_eq!(
            Group::from_san_string("g:cassini-client").unwrap(),
            Group("cassini-client".into())
        );
        for bad in [
            "encryption",
            "g:",
            "g:Encryption",
            "g:-x",
            "g:a b",
            "g:a:b",
            "c:grant",
            "p:s2s",
        ] {
            assert!(Group::from_san_string(bad).is_err(), "{bad}");
        }
    }

    proptest! {
        /// Every group name reads back as written.
        #[test]
        fn a_group_reads_back_as_written(name in "[a-z0-9][a-z0-9-]{0,62}") {
            let group = Group(name);
            prop_assert_eq!(Group::from_san_string(&group.to_san_string()).unwrap(), group);
        }
    }
}
