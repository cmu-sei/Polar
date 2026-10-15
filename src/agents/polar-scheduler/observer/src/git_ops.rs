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

//! Keeping the schedules checkout in step with its remote, with the `git` command.
//!
//! The command, not libgit2: the Polar git server knows who is asking from the client
//! certificate, and libgit2 cannot present one.

use anyhow::{Context, anyhow};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::info;

/// The certificates git uses to reach the remote. All unset: git's usual trust, no client
/// certificate.
#[derive(Debug, Clone, Default)]
pub struct GitTls {
    pub ca_cert: Option<PathBuf>,
    pub client_cert: Option<PathBuf>,
    pub client_key: Option<PathBuf>,
}

impl GitTls {
    fn command(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0");
        if let Some(ca) = &self.ca_cert {
            cmd.arg("-c")
                .arg(format!("http.sslCAInfo={}", ca.display()));
        }
        if let Some(cert) = &self.client_cert {
            cmd.arg("-c")
                .arg(format!("http.sslCert={}", cert.display()));
        }
        if let Some(key) = &self.client_key {
            cmd.arg("-c").arg(format!("http.sslKey={}", key.display()));
        }
        cmd
    }
}

fn run(mut cmd: Command, what: &str) -> Result<String, anyhow::Error> {
    let out = cmd
        .output()
        .with_context(|| format!("cannot run git {what}"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "git {what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Leaves a checkout of `remote_url` at `local_path`: an existing one is used as it is, a missing
/// one is cloned.
pub fn ensure_repo(local_path: &Path, remote_url: &str, tls: &GitTls) -> Result<(), anyhow::Error> {
    if local_path.join(".git").exists() {
        info!("Opening existing repo at {}", local_path.display());
        return Ok(());
    }
    info!("Cloning {} into {}", remote_url, local_path.display());
    let mut cmd = tls.command();
    cmd.arg("clone").arg(remote_url).arg(local_path);
    run(cmd, "clone").map(|_| ())
}

/// Brings the checkout up to its remote when that is a fast-forward. A history that has diverged
/// is an error and is left alone.
pub fn pull_repo(local_path: &Path, tls: &GitTls) -> Result<(), anyhow::Error> {
    info!("Pulling latest changes...");
    let mut cmd = tls.command();
    cmd.current_dir(local_path).args(["pull", "--ff-only"]);
    run(cmd, "pull").map(|_| ())
}

/// The repository a remote URL names: its last path segment without `.git`. A change announced
/// for any other repository is not about this checkout.
pub fn repo_name(remote_url: &str) -> Option<&str> {
    let last = remote_url.trim_end_matches('/').rsplit(['/', ':']).next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repository_is_the_last_path_segment() {
        assert_eq!(
            repo_name("https://polar-git-server.polar.svc.cluster.local:8443/polar-schedules.git"),
            Some("polar-schedules")
        );
        assert_eq!(
            repo_name("https://github.com/x/polar-schedules"),
            Some("polar-schedules")
        );
        assert_eq!(
            repo_name("git@github.com:x/polar-schedules.git"),
            Some("polar-schedules")
        );
        assert_eq!(repo_name("file:///tmp/r/"), Some("r"));
        assert_eq!(repo_name(""), None);
    }
}
