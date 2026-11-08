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

use cassini_client::TcpClientMessage;
use polar_scheduler_common::GitScheduleChange;
use polar_scheduler_observer::watcher::{GitWatcherActor, GitWatcherMsg};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::time::Duration;
use tempfile::TempDir;

struct MockTcpClient;

#[ractor::async_trait]
impl Actor for MockTcpClient {
    type Msg = TcpClientMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(&self, _: ActorRef<Self::Msg>, _: ()) -> Result<(), ActorProcessingErr> {
        Ok(())
    }
}

#[tokio::test]
async fn test_watcher_spawns() {
    let dir = TempDir::new().unwrap();

    let (tcp, _tcp_handle) = Actor::spawn(None, MockTcpClient, ()).await.unwrap();
    let (watcher, handle) = Actor::spawn(
        None,
        GitWatcherActor,
        (dir.path().to_string_lossy().to_string(), tcp),
    )
    .await
    .expect("GitWatcherActor should spawn");

    watcher.stop(None);
    handle.await.unwrap();
}

#[tokio::test]
async fn test_watcher_publish_change_create() {
    let dir = TempDir::new().unwrap();

    let (tcp, _tcp_handle) = Actor::spawn(None, MockTcpClient, ()).await.unwrap();
    let (watcher, handle) = Actor::spawn(
        None,
        GitWatcherActor,
        (dir.path().to_string_lossy().to_string(), tcp),
    )
    .await
    .unwrap();

    let change = GitScheduleChange::Create {
        path: "permanent/test.dhall".to_string(),
        json: "{}".to_string(),
    };
    watcher.cast(GitWatcherMsg::PublishChange(change)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    watcher.stop(None);
    handle.await.unwrap();
}

#[tokio::test]
async fn test_watcher_publish_change_update() {
    let dir = TempDir::new().unwrap();

    let (tcp, _tcp_handle) = Actor::spawn(None, MockTcpClient, ()).await.unwrap();
    let (watcher, handle) = Actor::spawn(
        None,
        GitWatcherActor,
        (dir.path().to_string_lossy().to_string(), tcp),
    )
    .await
    .unwrap();

    let change = GitScheduleChange::Update {
        path: "permanent/test.dhall".to_string(),
        json: "{}".to_string(),
    };
    watcher.cast(GitWatcherMsg::PublishChange(change)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    watcher.stop(None);
    handle.await.unwrap();
}

#[tokio::test]
async fn test_watcher_publish_change_delete() {
    let dir = TempDir::new().unwrap();

    let (tcp, _tcp_handle) = Actor::spawn(None, MockTcpClient, ()).await.unwrap();
    let (watcher, handle) = Actor::spawn(
        None,
        GitWatcherActor,
        (dir.path().to_string_lossy().to_string(), tcp),
    )
    .await
    .unwrap();

    let change = GitScheduleChange::Delete {
        path: "permanent/test.dhall".to_string(),
    };
    watcher.cast(GitWatcherMsg::PublishChange(change)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    watcher.stop(None);
    handle.await.unwrap();
}

#[tokio::test]
async fn test_watcher_perform_initial_scan_empty_dir() {
    let dir = TempDir::new().unwrap();

    let (tcp, _tcp_handle) = Actor::spawn(None, MockTcpClient, ()).await.unwrap();
    let (watcher, handle) = Actor::spawn(
        None,
        GitWatcherActor,
        (dir.path().to_string_lossy().to_string(), tcp),
    )
    .await
    .unwrap();

    watcher.cast(GitWatcherMsg::PerformInitialScan).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    watcher.stop(None);
    handle.await.unwrap();
}
