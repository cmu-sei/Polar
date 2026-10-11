//! Telling the rest of Polar that a repository changed.

use async_trait::async_trait;

/// One accepted ref update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoChanged {
    pub repo: String,
    pub refname: String,
    pub old: String,
    pub new: String,
    pub pusher: String,
}

/// Called after a push has been accepted by git, once per ref that actually moved. The Cassini
/// publisher implements this in the agent; tests use [`Recorder`].
#[async_trait]
pub trait Notifier: Send + Sync {
    async fn repo_changed(&self, change: RepoChanged);
}

/// Drops every notification.
pub struct Silent;

#[async_trait]
impl Notifier for Silent {
    async fn repo_changed(&self, _change: RepoChanged) {}
}

/// Keeps notifications for a test to inspect.
#[derive(Default)]
pub struct Recorder(pub std::sync::Mutex<Vec<RepoChanged>>);

#[async_trait]
impl Notifier for Recorder {
    async fn repo_changed(&self, change: RepoChanged) {
        self.0.lock().expect("recorder lock").push(change);
    }
}

/// Publishes each change on Cassini for the rest of Polar.
pub struct CassiniNotifier {
    client: ractor::ActorRef<cassini_client::TcpClientMessage>,
}

impl CassiniNotifier {
    pub fn new(client: ractor::ActorRef<cassini_client::TcpClientMessage>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Notifier for CassiniNotifier {
    async fn repo_changed(&self, change: RepoChanged) {
        use polar_git_server_common::{POLAR_GIT_REPO_CHANGED_TOPIC, RepoChangedEvent};
        let event = RepoChangedEvent {
            repo: change.repo,
            refname: change.refname,
            old: change.old,
            new: change.new,
            pusher: change.pusher,
        };
        let payload = match rkyv::to_bytes::<rkyv::rancor::Error>(&event) {
            Ok(bytes) => bytes.to_vec(),
            Err(e) => {
                tracing::error!(error = %e, "cannot encode a repository change");
                return;
            }
        };
        // The client buffers while the broker is away; a failure here means it is gone.
        if let Err(e) = self
            .client
            .send_message(cassini_client::TcpClientMessage::publish(
                POLAR_GIT_REPO_CHANGED_TOPIC.to_string(),
                payload,
                None,
            ))
        {
            tracing::error!(error = %e, repo = %event.repo, "cannot publish a repository change");
        }
    }
}

/// An append-only record of accepted pushes, one JSON object per line: when, who, which ref moved
/// from what to what. It is the server's own account of authorship; commits signed by their authors
/// are the account that travels with the data.
pub struct PushLog {
    path: std::path::PathBuf,
    // Serialises writers so a line is never interleaved with another.
    write: tokio::sync::Mutex<()>,
}

impl PushLog {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            path: path.into(),
            write: tokio::sync::Mutex::new(()),
        }
    }

    fn line(change: &RepoChanged, at_unix_secs: u64) -> String {
        serde_json::json!({
            "at": at_unix_secs,
            "repo": change.repo,
            "ref": change.refname,
            "old": change.old,
            "new": change.new,
            "pusher": change.pusher,
        })
        .to_string()
    }
}

#[async_trait]
impl Notifier for PushLog {
    async fn repo_changed(&self, change: RepoChanged) {
        use tokio::io::AsyncWriteExt;
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = format!("{}\n", Self::line(&change, at));
        let _guard = self.write.lock().await;
        let result = async {
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .await?;
            file.write_all(line.as_bytes()).await?;
            file.sync_data().await
        }
        .await;
        if let Err(e) = result {
            tracing::error!(error = %e, path = %self.path.display(), "cannot record a push");
        }
    }
}

/// Hands each change to every notifier in turn.
pub struct Tee(pub Vec<std::sync::Arc<dyn Notifier>>);

#[async_trait]
impl Notifier for Tee {
    async fn repo_changed(&self, change: RepoChanged) {
        for notifier in &self.0 {
            notifier.repo_changed(change.clone()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(new: &str) -> RepoChanged {
        RepoChanged {
            repo: "polar-schedules".into(),
            refname: "refs/heads/main".into(),
            old: "0".repeat(40),
            new: new.into(),
            pusher: "alice.polar.serviceaccount.cluster.local".into(),
        }
    }

    #[tokio::test]
    async fn pushes_are_appended_one_line_each_and_kept_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("push-log.jsonl");
        PushLog::new(&path)
            .repo_changed(change(&"a".repeat(40)))
            .await;
        // A new log object on the same file appends rather than truncating.
        let recorder = std::sync::Arc::new(Recorder::default());
        let tee = Tee(vec![
            std::sync::Arc::new(PushLog::new(&path)),
            recorder.clone(),
        ]);
        tee.repo_changed(change(&"b".repeat(40))).await;

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["new"], "a".repeat(40));
        assert_eq!(lines[1]["new"], "b".repeat(40));
        assert_eq!(lines[1]["repo"], "polar-schedules");
        assert_eq!(lines[1]["ref"], "refs/heads/main");
        assert_eq!(
            lines[1]["pusher"],
            "alice.polar.serviceaccount.cluster.local"
        );
        assert!(lines[1]["at"].as_u64().unwrap() > 1_700_000_000);
        // The other notifier still heard it.
        assert_eq!(recorder.0.lock().unwrap().len(), 1);
    }
}
