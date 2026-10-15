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

use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::path::PathBuf;
use std::time::Duration;
use tracing::{error, info};

use crate::git_ops::{GitTls, pull_repo};

pub struct GitSyncActor;

pub struct GitSyncArgs {
    pub repo_path: PathBuf,
    pub tls: GitTls,
    /// Pull on this interval as well; none means only [`SyncMessage::Now`] pulls.
    pub interval: Option<Duration>,
}

pub struct GitSyncState {
    repo_path: PathBuf,
    tls: GitTls,
}

#[derive(Debug, Clone)]
pub enum SyncMessage {
    /// The interval elapsed.
    Tick,
    /// The remote said it changed.
    Now,
}

#[ractor::async_trait]
impl Actor for GitSyncActor {
    type Msg = SyncMessage;
    type State = GitSyncState;
    type Arguments = GitSyncArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitSyncArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        match args.interval {
            Some(interval) => {
                info!(
                    "GitSyncActor starting, will pull every {}s and when told",
                    interval.as_secs()
                );
                myself.send_interval(interval, || SyncMessage::Tick);
            }
            None => info!("GitSyncActor starting, will pull when told"),
        }
        Ok(GitSyncState {
            repo_path: args.repo_path,
            tls: args.tls,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!(
            "GitSyncActor {msg:?} - pulling repo at {}",
            state.repo_path.display()
        );
        let path = state.repo_path.clone();
        let tls = state.tls.clone();
        // A failed pull is logged and the next one tries again.
        tokio::task::spawn_blocking(move || match pull_repo(&path, &tls) {
            Ok(()) => info!("Pull completed successfully"),
            Err(e) => error!("Failed to pull repo: {e:#}"),
        })
        .await
        .map_err(|e| ActorProcessingErr::from(e.to_string()))?;
        Ok(())
    }
}
