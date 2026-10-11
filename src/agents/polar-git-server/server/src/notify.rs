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
