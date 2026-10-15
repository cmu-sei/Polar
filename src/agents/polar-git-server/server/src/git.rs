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

//! Runs `git upload-pack` and `git receive-pack` in their stateless-RPC form for smart HTTP.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::pktline::pkt_line;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    UploadPack,
    ReceivePack,
}

impl Service {
    /// The `service` query value of `info/refs` (`git-upload-pack`).
    pub fn from_query(value: &str) -> Option<Self> {
        match value {
            "git-upload-pack" => Some(Self::UploadPack),
            "git-receive-pack" => Some(Self::ReceivePack),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::UploadPack => "git-upload-pack",
            Self::ReceivePack => "git-receive-pack",
        }
    }

    fn subcommand(self) -> &'static str {
        match self {
            Self::UploadPack => "upload-pack",
            Self::ReceivePack => "receive-pack",
        }
    }

    pub fn content_type(self) -> String {
        format!("application/x-{}-result", self.name())
    }

    pub fn advertisement_content_type(self) -> String {
        format!("application/x-{}-advertisement", self.name())
    }
}

#[derive(Debug, Error)]
pub enum GitError {
    #[error("git {0} timed out")]
    Timeout(&'static str),
    #[error("git {0} failed: {1}")]
    Failed(&'static str, String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct GitRunner {
    pub timeout: Duration,
}

impl Default for GitRunner {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(120),
        }
    }
}

impl GitRunner {
    /// The `GET info/refs?service=...` response body.
    pub async fn advertise(
        &self,
        repo: &Path,
        service: Service,
        git_protocol: Option<&str>,
    ) -> Result<Vec<u8>, GitError> {
        let refs = self
            .run(repo, service, &["--advertise-refs"], git_protocol, &[])
            .await?;
        let mut out = pkt_line(&format!("# service={}\n", service.name()));
        out.extend(b"0000");
        out.extend(refs);
        Ok(out)
    }

    /// The `POST git-<service>` response body for a request body.
    pub async fn rpc(
        &self,
        repo: &Path,
        service: Service,
        git_protocol: Option<&str>,
        body: &[u8],
    ) -> Result<Vec<u8>, GitError> {
        self.run(repo, service, &[], git_protocol, body).await
    }

    async fn run(
        &self,
        repo: &Path,
        service: Service,
        extra: &[&str],
        git_protocol: Option<&str>,
        stdin: &[u8],
    ) -> Result<Vec<u8>, GitError> {
        let what = service.subcommand();
        let mut cmd = Command::new("git");
        cmd.arg(what)
            .arg("--stateless-rpc")
            .args(extra)
            .arg(repo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(p) = git_protocol {
            cmd.env("GIT_PROTOCOL", p);
        }
        let mut child = cmd.spawn()?;
        let mut input = child.stdin.take().expect("stdin is piped");
        let body = stdin.to_vec();
        let feed = tokio::spawn(async move {
            // A process that exits early closes the pipe; its status says what happened.
            let _ = input.write_all(&body).await;
            let _ = input.shutdown().await;
        });
        let out = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| GitError::Timeout(what))??;
        let _ = feed.await;
        if !out.status.success() {
            return Err(GitError::Failed(
                what,
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        Ok(out.stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::RepoStore;

    #[test]
    fn only_the_two_services_are_recognised() {
        assert_eq!(
            Service::from_query("git-upload-pack"),
            Some(Service::UploadPack)
        );
        assert_eq!(
            Service::from_query("git-receive-pack"),
            Some(Service::ReceivePack)
        );
        assert_eq!(Service::from_query("git-shell"), None);
    }

    #[tokio::test]
    async fn the_advertisement_names_main_once_seeded() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tempfile::tempdir().unwrap();
        std::fs::write(tree.path().join("x"), "x").unwrap();
        let store = RepoStore::new(dir.path());
        store.create("r").await.unwrap();
        store.seed("r", tree.path()).await.unwrap();

        let body = GitRunner::default()
            .advertise(&store.path("r").unwrap(), Service::UploadPack, None)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.starts_with("001e# service=git-upload-pack\n0000"),
            "{text}"
        );
        assert!(text.contains("refs/heads/main"), "{text}");
    }
}
