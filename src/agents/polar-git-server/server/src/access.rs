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

//! Who may read and write which repository. Decisions go through [`Authorizer`] so a Cedar
//! evaluator can replace or override the static default without touching the server.

use crate::pktline::RefUpdate;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::RwLock;

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

    /// Whether `who` may administer the server (create repositories). Administrators may also do
    /// anything to every repository.
    fn is_admin(&self, who: &Identity) -> bool;
}

/// Who may do what to one repository.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
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

impl RepoAcl {
    fn allows(&self, who: &Identity, access: Access) -> bool {
        match access {
            Access::Write => self.writers.contains(&who.0),
            Access::Read => self.readers.contains(&who.0) || self.writers.contains(&who.0),
        }
    }
}

impl Authorizer for StaticAcl {
    fn allows(&self, who: &Identity, repo: &str, access: Access) -> bool {
        self.is_admin(who)
            || self
                .repos
                .get(repo)
                .is_some_and(|acl| acl.allows(who, access))
    }

    fn is_admin(&self, who: &Identity) -> bool {
        self.admins.contains(&who.0)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DeclareError {
    /// The deployment's configuration owns this repository's access list; runtime requests do not
    /// change it.
    Declared,
    Persist(String),
}

impl std::fmt::Display for DeclareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeclareError::Declared => {
                write!(
                    f,
                    "the deployment declares this repository; its access list is not changed here"
                )
            }
            DeclareError::Persist(e) => write!(f, "cannot save the access lists: {e}"),
        }
    }
}

/// The deployment's access lists plus those of repositories created at runtime. The deployment's
/// are the floor and always win; the runtime ones are saved to a file so they survive a restart.
#[derive(Debug)]
pub struct RuntimeAcl {
    base: StaticAcl,
    runtime: RwLock<HashMap<String, RepoAcl>>,
    file: Option<PathBuf>,
}

impl RuntimeAcl {
    /// `file` is where runtime access lists are kept; if it exists they are loaded from it.
    pub fn new(base: StaticAcl, file: Option<PathBuf>) -> Result<Self, String> {
        let runtime = match &file {
            Some(path) if path.is_file() => {
                let text = std::fs::read_to_string(path)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                serde_json::from_str(&text)
                    .map_err(|e| format!("{} is not a set of access lists: {e}", path.display()))?
            }
            _ => HashMap::new(),
        };
        Ok(Self {
            base,
            runtime: RwLock::new(runtime),
            file,
        })
    }

    /// Sets the access list of a runtime repository, replacing any earlier one. Returns an error
    /// for a repository the deployment declares.
    pub fn declare(&self, repo: &str, acl: RepoAcl) -> Result<(), DeclareError> {
        if self.base.repos.contains_key(repo) {
            return Err(DeclareError::Declared);
        }
        let mut runtime = self.runtime.write().expect("acl lock");
        let previous = runtime.insert(repo.to_string(), acl);
        if let Err(e) = self.persist(&runtime) {
            match previous {
                Some(p) => runtime.insert(repo.to_string(), p),
                None => runtime.remove(repo),
            };
            return Err(DeclareError::Persist(e));
        }
        Ok(())
    }

    fn persist(&self, runtime: &HashMap<String, RepoAcl>) -> Result<(), String> {
        let Some(path) = &self.file else {
            return Ok(());
        };
        let text = serde_json::to_string_pretty(runtime).map_err(|e| e.to_string())?;
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, text)
            .and_then(|()| std::fs::rename(&temp, path))
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}

impl Authorizer for RuntimeAcl {
    fn allows(&self, who: &Identity, repo: &str, access: Access) -> bool {
        if self.base.allows(who, repo, access) {
            return true;
        }
        // The deployment's list for a declared repository is complete.
        !self.base.repos.contains_key(repo)
            && self
                .runtime
                .read()
                .expect("acl lock")
                .get(repo)
                .is_some_and(|acl| acl.allows(who, access))
    }

    fn is_admin(&self, who: &Identity) -> bool {
        self.base.is_admin(who)
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

    #[test]
    fn a_runtime_repository_is_listed_survives_a_restart_and_never_overrides_the_deployment() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("runtime-acl.json");
        let acl = RuntimeAcl::new(acl(), Some(file.clone())).unwrap();
        let fresh = RepoAcl {
            readers: ["bob".to_string()].into(),
            writers: ["carol".to_string()].into(),
        };
        assert!(!acl.allows(&who("bob"), "scratch", Access::Read));
        acl.declare("scratch", fresh.clone()).unwrap();
        assert!(acl.allows(&who("bob"), "scratch", Access::Read));
        assert!(!acl.allows(&who("bob"), "scratch", Access::Write));
        assert!(acl.allows(&who("carol"), "scratch", Access::Write));
        assert!(acl.allows(&who("root"), "scratch", Access::Write));

        // Declaring again replaces; a restart reads it back.
        acl.declare("scratch", RepoAcl::default()).unwrap();
        assert!(!acl.allows(&who("bob"), "scratch", Access::Read));
        acl.declare("scratch", fresh).unwrap();
        let restarted = RuntimeAcl::new(acl_base(), Some(file)).unwrap();
        assert!(restarted.allows(&who("carol"), "scratch", Access::Write));

        // The deployment owns what it declares.
        assert_eq!(
            acl.declare("polar-schedules", RepoAcl::default()),
            Err(DeclareError::Declared)
        );
        assert!(acl.allows(&who("alice"), "polar-schedules", Access::Write));
    }

    fn acl_base() -> StaticAcl {
        acl()
    }
}
