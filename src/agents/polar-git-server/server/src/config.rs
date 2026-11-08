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

//! The server's declaration of what it hosts: repositories, who may read and write them, and what
//! each is seeded from. The deployment renders this file from the catalog; the server applies it at
//! start. It is a floor, not the whole list: repositories created later at runtime are not in it.

use crate::access::{RepoAcl, StaticAcl};
use crate::store::{RepoStore, Seeded, StoreError};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tracing::info;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerFile {
    /// Identities that may do anything to every repository.
    #[serde(default)]
    pub admins: Vec<String>,
    #[serde(default)]
    pub repos: Vec<RepoSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoSpec {
    pub name: String,
    #[serde(default)]
    pub readers: Vec<String>,
    #[serde(default)]
    pub writers: Vec<String>,
    /// A directory committed as the first commit when the repository is empty. A relative path is
    /// resolved against the server's seed root.
    pub seed: Option<PathBuf>,
}

impl ServerFile {
    pub fn parse(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("the git server's configuration file: {e}"))
    }

    /// The same file with more administrators: those the deployment names apart from the file.
    pub fn with_admins(mut self, more: impl IntoIterator<Item = String>) -> Self {
        for admin in more {
            if !self.admins.contains(&admin) {
                self.admins.push(admin);
            }
        }
        self
    }

    pub fn acl(&self) -> StaticAcl {
        StaticAcl {
            admins: self.admins.iter().cloned().collect(),
            repos: self
                .repos
                .iter()
                .map(|r| {
                    (
                        r.name.clone(),
                        RepoAcl {
                            readers: r.readers.iter().cloned().collect(),
                            writers: r.writers.iter().cloned().collect(),
                        },
                    )
                })
                .collect(),
        }
    }

    /// Creates every declared repository that is missing and seeds the empty ones. Safe to run
    /// at every start: existing history is never touched.
    pub async fn provision(&self, store: &RepoStore, seed_root: &Path) -> Result<(), StoreError> {
        for repo in &self.repos {
            let created = store.create(&repo.name).await?;
            let seeded = match &repo.seed {
                Some(tree) => Some(store.seed(&repo.name, &seed_root.join(tree)).await?),
                None => None,
            };
            info!(repo = %repo.name, ?created, ?seeded, "repository provisioned");
            let _: Option<Seeded> = seeded;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{Access, Authorizer, Identity};

    const FILE: &str = r#"{
        "admins": ["root"],
        "repos": [
            { "name": "polar-schedules", "readers": ["polar-scheduler-observer"], "writers": ["alice"] },
            { "name": "empty" }
        ]
    }"#;

    #[test]
    fn administrators_named_apart_from_the_file_are_added_to_its_own() {
        let file = ServerFile::parse(r#"{ "admins": ["root"], "repos": [] }"#)
            .unwrap()
            .with_admins(["uuid-one".to_string(), "root".to_string()]);
        let acl = file.acl();
        assert!(acl.admins.contains("root") && acl.admins.contains("uuid-one"));
        assert_eq!(acl.admins.len(), 2, "a name in both is one administrator");
    }

    #[test]
    fn the_file_becomes_the_acl() {
        let acl = ServerFile::parse(FILE).unwrap().acl();
        let id = |s: &str| Identity(s.to_string());
        assert!(acl.allows(
            &id("polar-scheduler-observer"),
            "polar-schedules",
            Access::Read
        ));
        assert!(!acl.allows(
            &id("polar-scheduler-observer"),
            "polar-schedules",
            Access::Write
        ));
        assert!(acl.allows(&id("alice"), "polar-schedules", Access::Write));
        assert!(acl.allows(&id("root"), "empty", Access::Write));
        assert!(!acl.allows(&id("alice"), "empty", Access::Read));
    }

    #[test]
    fn a_misspelled_key_is_an_error_not_a_silent_default() {
        assert!(ServerFile::parse(r#"{ "repoz": [] }"#).is_err());
        assert!(ServerFile::parse(r#"{ "repos": [ { "name": "a", "writer": [] } ] }"#).is_err());
    }

    #[tokio::test]
    async fn provisioning_creates_seeds_and_repeats_safely() {
        let root = tempfile::tempdir().unwrap();
        let seed = tempfile::tempdir().unwrap();
        std::fs::write(seed.path().join("a.dhall"), "1").unwrap();
        let file = ServerFile {
            admins: vec![],
            repos: vec![
                RepoSpec {
                    name: "seeded".into(),
                    readers: vec![],
                    writers: vec![],
                    seed: Some(seed.path().to_path_buf()),
                },
                RepoSpec {
                    name: "bare".into(),
                    readers: vec![],
                    writers: vec![],
                    seed: None,
                },
            ],
        };
        let store = RepoStore::new(root.path());
        file.provision(&store, Path::new("/nowhere")).await.unwrap();
        assert!(store.has_history("seeded").await.unwrap());
        assert!(store.exists("bare"));
        assert!(!store.has_history("bare").await.unwrap());

        // A restart with the seed changed leaves the history alone.
        std::fs::write(seed.path().join("b.dhall"), "2").unwrap();
        file.provision(&store, Path::new("/nowhere")).await.unwrap();
        let head = store.resolve_ref("seeded", "refs/heads/main").await;
        file.provision(&store, Path::new("/nowhere")).await.unwrap();
        assert_eq!(store.resolve_ref("seeded", "refs/heads/main").await, head);
    }

    #[tokio::test]
    async fn a_relative_seed_is_taken_from_the_seed_root() {
        let root = tempfile::tempdir().unwrap();
        let seeds = tempfile::tempdir().unwrap();
        std::fs::create_dir(seeds.path().join("polar-schedules")).unwrap();
        std::fs::write(seeds.path().join("polar-schedules/README.md"), "hi").unwrap();
        let file = ServerFile::parse(
            r#"{ "repos": [ { "name": "polar-schedules", "seed": "polar-schedules" } ] }"#,
        )
        .unwrap();
        let store = RepoStore::new(root.path());
        file.provision(&store, seeds.path()).await.unwrap();
        assert!(store.has_history("polar-schedules").await.unwrap());
    }

    /// The trees the deployment ships (infra/seeds) are what the catalog's `seed` names refer to.
    #[tokio::test]
    async fn the_seeds_the_deployment_ships_provision_a_repository_each() {
        let seeds = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../infra/seeds");
        let names: Vec<String> = std::fs::read_dir(&seeds)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(names.contains(&"polar-schedules".to_string()), "{names:?}");
        let root = tempfile::tempdir().unwrap();
        let store = RepoStore::new(root.path());
        for name in &names {
            let file = ServerFile::parse(&format!(
                r#"{{ "repos": [ {{ "name": "{name}", "seed": "{name}" }} ] }}"#
            ))
            .unwrap();
            file.provision(&store, &seeds).await.unwrap();
            assert!(store.has_history(name).await.unwrap(), "{name}");
        }
    }
}
