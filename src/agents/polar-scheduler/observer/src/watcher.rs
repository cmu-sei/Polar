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

use crate::held::HeldDocuments;
use cassini_client::TcpClientMessage;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use polar_scheduler_common::{
    ConfigurationNotification, GitScheduleChange, SCHEDULER_IN_TOPIC, configuration_topic,
};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::collections::HashSet;
use std::path::Path;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

pub struct GitWatcherActor;

pub struct GitWatcherState {
    tcp_client: ActorRef<TcpClientMessage>,
    repo_path: String,
    /// The signed documents in the checkout, kept current as the watcher announces each change.
    held: HeldDocuments,
    /// True once the initial scan has read the whole checkout. Until then `held` is not the repository's
    /// word, so a request is not answered from it.
    scanned: bool,
    /// Agents that asked before the scan finished; each is answered once, when it has.
    waiting: HashSet<String>,
}

impl GitWatcherState {
    /// Sends `agent` what the checkout holds for it, on its own configuration topic. The agent
    /// verifies each document like any other and refuses one it has already applied.
    fn replay(&self, agent: &str) -> Result<(), ActorProcessingErr> {
        let documents = self.held.for_agent(agent);
        info!(
            agent,
            count = documents.len(),
            "replaying held configuration"
        );
        for document_json in documents {
            let notification = ConfigurationNotification {
                agent: agent.to_string(),
                document_json,
            };
            let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&notification)?.to_vec();
            self.tcp_client.cast(TcpClientMessage::publish(
                configuration_topic(agent),
                payload,
                None,
            ))?;
        }
        Ok(())
    }
}

impl GitWatcherActor {
    fn evaluate_dhall_file_to_json(file_path: &Path) -> Result<String, ActorProcessingErr> {
        // What is committed is what polar-sign rendered, with nothing left to resolve: a file with an import is refused
        // here, before anything it names is read or asked for (docs/architecture/signed-configuration.md).
        let simple: serde_dhall::SimpleValue = serde_dhall::from_file(file_path)
            .imports(false)
            .parse()
            .map_err(|e| ActorProcessingErr::from(format!("Dhall error: {}", e)))?;
        let json = serde_json::to_value(&simple)
            .map_err(|e| ActorProcessingErr::from(format!("JSON conversion error: {}", e)))?;
        Ok(serde_json::to_string(&json)?)
    }

    fn is_schedule_file(path: &Path, repo_path: &str) -> bool {
        let repo_path = Path::new(repo_path);
        let relative = path.strip_prefix(repo_path).unwrap_or(path);
        relative.components().any(|c| {
            let s = c.as_os_str().to_string_lossy();
            s == "permanent" || s == "adhoc" || s == "ephemeral"
        })
    }

    async fn start_filesystem_watcher(
        repo_path: String,
        myself: ActorRef<GitWatcherMsg>,
    ) -> Result<(), ActorProcessingErr> {
        let (tx, mut rx) = mpsc::channel(100);
        let path = repo_path.clone();

        // Spawn a blocking thread for notify (it's synchronous)
        std::thread::spawn(move || {
            let mut watcher: RecommendedWatcher = Watcher::new(
                move |res: notify::Result<notify::Event>| {
                    if let Ok(event) = res {
                        let _ = tx.blocking_send(event);
                    }
                },
                notify::Config::default(),
            )
            .unwrap();
            watcher
                .watch(Path::new(&path), RecursiveMode::Recursive)
                .unwrap();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        });

        // Process filesystem events
        while let Some(event) = rx.recv().await {
            for path in event.paths {
                if path.extension().and_then(|s| s.to_str()) == Some("dhall")
                    && Self::is_schedule_file(&path, &repo_path)
                {
                    let path_str = path.to_string_lossy().to_string();

                    // Handle delete immediately
                    if event.kind.is_remove() {
                        let change = GitScheduleChange::Delete { path: path_str };
                        myself.cast(GitWatcherMsg::PublishChange(change))?;
                        continue;
                    }

                    // For create/modify, evaluate the file directly
                    if event.kind.is_create() || event.kind.is_modify() {
                        let path_clone = path.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            Self::evaluate_dhall_file_to_json(&path_clone)
                        })
                        .await;

                        match result {
                            Ok(Ok(json)) => {
                                let change = if event.kind.is_create() {
                                    GitScheduleChange::Create {
                                        path: path_str,
                                        json,
                                    }
                                } else {
                                    GitScheduleChange::Update {
                                        path: path_str,
                                        json,
                                    }
                                };
                                myself.cast(GitWatcherMsg::PublishChange(change))?;
                            }
                            Ok(Err(e)) => {
                                error!("Failed to evaluate Dhall file {}: {:?}", path_str, e);
                            }
                            Err(e) => error!("Task join error for {}: {}", path_str, e),
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn initial_scan(
        repo_path: String,
        myself: ActorRef<GitWatcherMsg>,
    ) -> Result<(), ActorProcessingErr> {
        info!("Performing initial scan of {}", repo_path);
        let walker = walkdir::WalkDir::new(&repo_path)
            .follow_links(true)
            .into_iter()
            .filter_entry(|e| {
                !e.file_name()
                    .to_str()
                    .map(|s| s.starts_with('.'))
                    .unwrap_or(false)
            });
        for entry in walker {
            let entry = entry?;
            if entry.file_type().is_file()
                && let Some(ext) = entry.path().extension()
                && ext == "dhall"
            {
                let path = entry.path().to_path_buf();
                if !Self::is_schedule_file(&path, &repo_path) {
                    continue;
                }
                let path_str = path.to_string_lossy().to_string();
                let path_clone = path.clone();
                let result = tokio::task::spawn_blocking(move || {
                    Self::evaluate_dhall_file_to_json(&path_clone)
                })
                .await;
                match result {
                    Ok(Ok(json)) => {
                        let change = GitScheduleChange::Create {
                            path: path_str,
                            json,
                        };
                        myself.cast(GitWatcherMsg::PublishChange(change))?;
                    }
                    Ok(Err(e)) => {
                        error!("Failed to evaluate Dhall file {}: {:?}", path_str, e)
                    }
                    Err(e) => error!("Task join error for {}: {}", path_str, e),
                }
            }
        }
        // Everything the scan found was cast to the watcher before this, and a mailbox is in order.
        myself.cast(GitWatcherMsg::InitialScanComplete)?;
        Ok(())
    }
}

#[derive(Debug)]
pub enum GitWatcherMsg {
    PublishChange(GitScheduleChange),
    PerformInitialScan,
    /// The initial scan has announced everything in the checkout.
    InitialScanComplete,
    /// An agent that has just registered asks for the signed configuration held for it.
    Replay {
        agent: String,
    },
}

#[ractor::async_trait]
impl Actor for GitWatcherActor {
    type Msg = GitWatcherMsg;
    type State = GitWatcherState;
    type Arguments = (String, ActorRef<TcpClientMessage>);

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        (repo_path, tcp_client): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        info!("GitWatcher starting, watching {}", repo_path);
        Ok(GitWatcherState {
            tcp_client,
            repo_path,
            held: HeldDocuments::default(),
            scanned: false,
            waiting: HashSet::new(),
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // Start filesystem watcher in background
        let myself_clone = myself.clone();
        let repo_path = state.repo_path.clone();
        tokio::spawn(async move {
            if let Err(e) = Self::start_filesystem_watcher(repo_path, myself_clone).await {
                error!("Filesystem watcher error: {:?}", e);
            }
        });

        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            GitWatcherMsg::PublishChange(change) => {
                match &change {
                    GitScheduleChange::Create { path, json }
                    | GitScheduleChange::Update { path, json } => state.held.record(path, json),
                    GitScheduleChange::Delete { path } => state.held.remove(path),
                }
                let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&change)?.to_vec();
                state.tcp_client.cast(TcpClientMessage::publish(
                    SCHEDULER_IN_TOPIC.to_string(),
                    payload,
                    None,
                ))?;
                debug!("Published schedule change");
            }
            GitWatcherMsg::PerformInitialScan => {
                let myself = _myself.clone();
                let repo_path = state.repo_path.clone();
                tokio::spawn(async move {
                    if let Err(e) = Self::initial_scan(repo_path, myself).await {
                        error!("Initial scan error: {:?}", e);
                    }
                });
            }
            GitWatcherMsg::InitialScanComplete => {
                state.scanned = true;
                for agent in std::mem::take(&mut state.waiting) {
                    state.replay(&agent)?;
                }
            }
            GitWatcherMsg::Replay { agent } => {
                if state.scanned {
                    state.replay(&agent)?;
                } else {
                    debug!(
                        agent,
                        "asked before the scan finished; answering when it has"
                    );
                    state.waiting.insert(agent);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::net::TcpListener;
    use tempfile::TempDir;

    /// What the observer makes of a file holding `source`.
    fn evaluated(dir: &TempDir, source: &str) -> Result<String, ActorProcessingErr> {
        let file = dir.path().join("document.dhall");
        std::fs::write(&file, source).unwrap();
        GitWatcherActor::evaluate_dhall_file_to_json(&file)
    }

    /// A signed document as `polar-sign` writes it: nothing left to resolve, and it evaluates.
    #[test]
    fn a_file_with_nothing_to_resolve_evaluates() {
        let dir = TempDir::new().unwrap();
        let json = evaluated(
            &dir,
            r#"{ payload = { id = "one", version = 3 }, signature = { chain = [ "a", "b" ] } }"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap(),
            serde_json::json!({
                "payload": { "id": "one", "version": 3 },
                "signature": { "chain": ["a", "b"] }
            })
        );
    }

    /// A committed file is what `polar-sign` rendered, with nothing left to resolve (docs/architecture/
    /// signed-configuration.md), so an import is refused wherever it stands, even where evaluation would not reach it.
    #[test]
    fn an_import_is_refused_however_it_is_written() {
        let dir = TempDir::new().unwrap();
        let held = dir.path().join("held.txt");
        std::fs::write(&held, "HELD-IN-THE-FILE").unwrap();
        let held = held.display();
        for source in [
            format!("{{ a = {held} as Text }}"),
            format!("{held}"),
            "./other.dhall".to_string(),
            "../other.dhall".to_string(),
            "~/other.dhall".to_string(),
            "env:HOME as Text".to_string(),
            "env:HOME".to_string(),
            "missing".to_string(),
            format!("if False then {held} as Text else \"unreached\""),
            format!("let unused = {held} as Text in 1"),
            format!("[ {held} as Text ]"),
            format!(
                "{held} sha256:0000000000000000000000000000000000000000000000000000000000000000"
            ),
        ] {
            match evaluated(&dir, &source) {
                Ok(json) => panic!("`{source}` was evaluated, to {json}"),
                Err(why) => assert!(
                    !why.to_string().contains("HELD-IN-THE-FILE"),
                    "`{source}`: {why}"
                ),
            }
        }
    }

    /// Dhall's fallback, `import ? other`, takes its other side when the import is refused, so such a file evaluates, to
    /// what it says in plain text: what the import names is still not read.
    #[test]
    fn a_fallback_is_all_that_is_left_of_an_import() {
        let dir = TempDir::new().unwrap();
        let held = dir.path().join("held.txt");
        std::fs::write(&held, "HELD-IN-THE-FILE").unwrap();
        let held = held.display();
        for (source, expected) in [
            (format!("{held} as Text ? \"other\""), "\"other\""),
            (
                format!("{held} as Text ? env:HOME as Text ? \"other\""),
                "\"other\"",
            ),
            ("missing ? 1".to_string(), "1"),
        ] {
            assert_eq!(evaluated(&dir, &source).unwrap(), expected, "`{source}`");
        }
    }

    /// Refused before anything is asked for: a file does not make the observer connect anywhere.
    #[test]
    fn an_address_in_a_file_is_not_asked() {
        let dir = TempDir::new().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        for source in [
            format!("http://127.0.0.1:{port}/asked as Text"),
            format!("{{ a = http://127.0.0.1:{port}/asked }}"),
        ] {
            assert!(evaluated(&dir, &source).is_err(), "`{source}`");
        }
        // Nor does one with a fallback.
        assert_eq!(
            evaluated(
                &dir,
                &format!("http://127.0.0.1:{port}/asked as Text ? \"other\"")
            )
            .unwrap(),
            "\"other\""
        );
        let asked = listener.accept();
        assert!(
            matches!(&asked, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "the observer connected to an address a file named: {asked:?}"
        );
    }

    fn word() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9]{0,8}"
    }

    proptest! {
        /// What a file with nothing to resolve says is what comes out.
        #[test]
        fn a_plain_record_comes_out_as_it_went_in(
            fields in proptest::collection::btree_map(word(), word(), 1..6),
        ) {
            let dir = TempDir::new().unwrap();
            let body: Vec<String> = fields.iter().map(|(k, v)| format!("{k}x = \"{v}\"")).collect();
            let json = evaluated(&dir, &format!("{{ {} }}", body.join(", "))).unwrap();
            let expected: serde_json::Map<String, serde_json::Value> = fields
                .iter()
                .map(|(k, v)| (format!("{k}x"), serde_json::Value::String(v.clone())))
                .collect();
            prop_assert_eq!(
                serde_json::from_str::<serde_json::Value>(&json).unwrap(),
                serde_json::Value::Object(expected)
            );
        }

        /// However many plain fields stand around it, one import refuses the whole file.
        #[test]
        fn one_import_refuses_the_whole_file(
            fields in proptest::collection::btree_map(word(), word(), 0..6),
            position in 0usize..6,
            import in prop_oneof![
                Just("./other.dhall"),
                Just("env:HOME as Text"),
                Just("missing"),
                Just("http://127.0.0.1:9/asked as Text"),
            ],
        ) {
            let dir = TempDir::new().unwrap();
            let mut body: Vec<String> = fields.iter().map(|(k, v)| format!("{k}x = \"{v}\"")).collect();
            let at = position.min(body.len());
            body.insert(at, format!("imported = {import}"));
            let source = format!("{{ {} }}", body.join(", "));
            prop_assert!(evaluated(&dir, &source).is_err(), "`{}`", source);
        }
    }
}
