//! Who may read and write which repository. Decisions go through [`Authorizer`] so a Cedar
//! evaluator can replace or override the static default without touching the server.

use crate::pktline::RefUpdate;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

/// The peer identity taken from the verified TLS handshake.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Identity(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

pub trait Authorizer: Send + Sync {
    fn allows(&self, who: &Identity, repo: &str, access: Access) -> bool;
}

/// Who may do what to one repository.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RepoAcl {
    #[serde(default)]
    pub readers: HashSet<String>,
    #[serde(default)]
    pub writers: HashSet<String>,
}

/// The default policy: per-repository lists, plus administrators who may do anything. A writer
/// can read. A repository that is not listed is denied to everyone but administrators.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StaticAcl {
    #[serde(default)]
    pub admins: HashSet<String>,
    #[serde(default)]
    pub repos: HashMap<String, RepoAcl>,
}

impl Authorizer for StaticAcl {
    fn allows(&self, who: &Identity, repo: &str, access: Access) -> bool {
        if self.admins.contains(&who.0) {
            return true;
        }
        let Some(acl) = self.repos.get(repo) else {
            return false;
        };
        match access {
            Access::Write => acl.writers.contains(&who.0),
            Access::Read => acl.readers.contains(&who.0) || acl.writers.contains(&who.0),
        }
    }
}

/// Authorizes a push. Every command must be to a branch (`refs/heads/`); tags and other
/// namespaces are not part of the configuration model. Returns the refusal, if any.
pub fn authorize_push(
    authorizer: &dyn Authorizer,
    who: &Identity,
    repo: &str,
    updates: &[RefUpdate],
) -> Result<(), String> {
    if !authorizer.allows(who, repo, Access::Write) {
        return Err(format!("{} may not write {repo}", who.0));
    }
    if let Some(bad) = updates
        .iter()
        .find(|u| !u.refname.starts_with("refs/heads/"))
    {
        return Err(format!("{} is not a branch", bad.refname));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acl() -> StaticAcl {
        let mut repos = HashMap::new();
        repos.insert(
            "polar-schedules".to_string(),
            RepoAcl {
                readers: ["polar-scheduler-observer".to_string()].into(),
                writers: ["alice".to_string()].into(),
            },
        );
        StaticAcl {
            admins: ["root".to_string()].into(),
            repos,
        }
    }

    fn who(s: &str) -> Identity {
        Identity(s.to_string())
    }

    #[test]
    fn readers_read_but_do_not_write() {
        let a = acl();
        assert!(a.allows(
            &who("polar-scheduler-observer"),
            "polar-schedules",
            Access::Read
        ));
        assert!(!a.allows(
            &who("polar-scheduler-observer"),
            "polar-schedules",
            Access::Write
        ));
    }

    #[test]
    fn writers_read_and_write() {
        let a = acl();
        assert!(a.allows(&who("alice"), "polar-schedules", Access::Read));
        assert!(a.allows(&who("alice"), "polar-schedules", Access::Write));
    }

    #[test]
    fn strangers_and_unlisted_repos_are_denied() {
        let a = acl();
        assert!(!a.allows(&who("mallory"), "polar-schedules", Access::Read));
        assert!(!a.allows(&who("alice"), "other", Access::Read));
    }

    #[test]
    fn admins_may_do_anything() {
        let a = acl();
        assert!(a.allows(&who("root"), "other", Access::Write));
    }

    #[test]
    fn a_push_to_a_non_branch_is_refused() {
        let a = acl();
        let tag = RefUpdate {
            old: "0".repeat(40),
            new: "a".repeat(40),
            refname: "refs/tags/v1".into(),
        };
        assert!(authorize_push(&a, &who("alice"), "polar-schedules", &[tag]).is_err());
        let main = RefUpdate {
            old: "0".repeat(40),
            new: "a".repeat(40),
            refname: "refs/heads/main".into(),
        };
        assert!(
            authorize_push(
                &a,
                &who("alice"),
                "polar-schedules",
                std::slice::from_ref(&main)
            )
            .is_ok()
        );
        assert!(authorize_push(&a, &who("mallory"), "polar-schedules", &[main]).is_err());
    }
}
