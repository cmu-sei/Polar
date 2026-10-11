//! The bare repositories on disk: creation, and one-time seeding from a directory.

use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("'{0}' is not a valid repository name")]
    BadName(String),
    #[error("repository '{0}' does not exist")]
    Missing(String),
    #[error("git {0} failed: {1}")]
    Git(&'static str, String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Created {
    New,
    AlreadyExisted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seeded {
    Applied,
    /// The repository already has history; it is left alone.
    Skipped,
}

#[derive(Debug, Clone)]
pub struct RepoStore {
    root: PathBuf,
}

/// Lower-case letters, digits, '-', '_' and '.', not starting with '.', no ".." and at most 64
/// characters. Keeps a name from escaping the root or colliding with git's own files.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && !name.contains("..")
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
        })
}

impl RepoStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Where the repository lives; its existence is not checked.
    pub fn path(&self, name: &str) -> Result<PathBuf, StoreError> {
        if !valid_name(name) {
            return Err(StoreError::BadName(name.to_string()));
        }
        Ok(self.root.join(format!("{name}.git")))
    }

    pub fn exists(&self, name: &str) -> bool {
        self.path(name)
            .map(|p| p.join("HEAD").is_file())
            .unwrap_or(false)
    }

    /// Creates an empty bare repository whose branch is `main`. Idempotent.
    pub async fn create(&self, name: &str) -> Result<Created, StoreError> {
        let path = self.path(name)?;
        if self.exists(name) {
            return Ok(Created::AlreadyExisted);
        }
        tokio::fs::create_dir_all(&path).await?;
        run_git(
            &path,
            None,
            &["init", "--bare", "--initial-branch=main", "."],
            "init",
        )
        .await?;
        // Pushes are checked by the server; do not let a client rewrite history by accident.
        run_git(
            &path,
            None,
            &["config", "receive.denyNonFastForwards", "true"],
            "config",
        )
        .await?;
        run_git(
            &path,
            None,
            &["config", "receive.denyDeletes", "true"],
            "config",
        )
        .await?;
        Ok(Created::New)
    }

    /// Commits the contents of `tree` as the first commit on `main`, only when the repository has
    /// no refs, so a redeployment never overwrites later edits.
    pub async fn seed(&self, name: &str, tree: &Path) -> Result<Seeded, StoreError> {
        if !self.exists(name) {
            return Err(StoreError::Missing(name.to_string()));
        }
        let path = self.path(name)?;
        if self.has_history(name).await? {
            return Ok(Seeded::Skipped);
        }
        run_git(&path, Some(tree), &["add", "--all"], "add").await?;
        run_git(
            &path,
            Some(tree),
            &[
                "-c",
                "user.name=polar-git-server",
                "-c",
                "user.email=polar-git-server@localhost",
                "commit",
                "--allow-empty",
                "--message",
                "chore: seed from deployment",
            ],
            "commit",
        )
        .await?;
        Ok(Seeded::Applied)
    }

    /// The object a ref points at, if it exists.
    pub async fn resolve_ref(&self, name: &str, refname: &str) -> Option<String> {
        let path = self.path(name).ok()?;
        let spec = format!("{refname}^{{commit}}");
        run_git(
            &path,
            None,
            &["rev-parse", "--verify", "--quiet", &spec],
            "rev-parse",
        )
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    }

    pub async fn has_history(&self, name: &str) -> Result<bool, StoreError> {
        let path = self.path(name)?;
        let out = run_git(&path, None, &["for-each-ref", "--count=1"], "for-each-ref").await?;
        Ok(!out.trim().is_empty())
    }
}

/// Runs git against `git_dir`, with `work_tree` when given. Returns stdout.
async fn run_git(
    git_dir: &Path,
    work_tree: Option<&Path>,
    args: &[&str],
    what: &'static str,
) -> Result<String, StoreError> {
    let mut cmd = Command::new("git");
    cmd.env("GIT_DIR", git_dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    if let Some(tree) = work_tree {
        cmd.env("GIT_WORK_TREE", tree).current_dir(tree);
    } else {
        cmd.current_dir(git_dir);
    }
    let out = cmd.args(args).output().await?;
    if !out.status.success() {
        return Err(StoreError::Git(
            what,
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_cannot_escape_or_collide() {
        for bad in [
            "",
            ".hidden",
            "a/b",
            "..",
            "a..b",
            "Upper",
            "a b",
            &"x".repeat(65),
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        for good in ["polar-schedules", "agent_cfg.v2", "a"] {
            assert!(valid_name(good), "{good:?}");
        }
    }

    #[tokio::test]
    async fn create_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = RepoStore::new(dir.path());
        assert!(!store.exists("polar-schedules"));
        assert_eq!(store.create("polar-schedules").await.unwrap(), Created::New);
        assert!(store.exists("polar-schedules"));
        assert_eq!(
            store.create("polar-schedules").await.unwrap(),
            Created::AlreadyExisted
        );
    }

    #[tokio::test]
    async fn a_bad_name_is_refused_before_touching_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = RepoStore::new(dir.path());
        assert!(matches!(
            store.create("../x").await,
            Err(StoreError::BadName(_))
        ));
    }

    #[tokio::test]
    async fn seeding_happens_once() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tempfile::tempdir().unwrap();
        std::fs::write(tree.path().join("a.dhall"), "1").unwrap();
        let store = RepoStore::new(dir.path());
        store.create("r").await.unwrap();
        assert!(!store.has_history("r").await.unwrap());
        assert_eq!(store.seed("r", tree.path()).await.unwrap(), Seeded::Applied);
        assert!(store.has_history("r").await.unwrap());

        // A later deploy with different content leaves the history alone.
        std::fs::write(tree.path().join("b.dhall"), "2").unwrap();
        assert_eq!(store.seed("r", tree.path()).await.unwrap(), Seeded::Skipped);
        let log = run_git(
            &store.path("r").unwrap(),
            None,
            &["log", "--oneline"],
            "log",
        )
        .await
        .unwrap();
        assert_eq!(log.lines().count(), 1);
    }

    #[tokio::test]
    async fn seeding_a_missing_repository_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = RepoStore::new(dir.path());
        assert!(matches!(
            store.seed("nope", dir.path()).await,
            Err(StoreError::Missing(_))
        ));
    }
}
