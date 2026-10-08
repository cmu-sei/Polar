//! What the broker does when Kubernetes asks it to stop.
//!
//! A pod that is deleted gets SIGTERM, then SIGKILL when its grace period runs out. Without a
//! handler SIGTERM ends the process on the spot, and the orderly shutdown (stop accepting,
//! drain sessions, flush topics, close subscribers and listeners) never runs.

use cassini_types::BrokerMessage;
use ractor::ActorRef;
use tracing::{error, info};

/// Starts the broker's orderly shutdown when the process receives SIGTERM or SIGINT. `token` is
/// the shutdown token the broker was given, which its control manager checks.
pub fn begin_shutdown_on_signal(broker: ActorRef<BrokerMessage>, token: Option<String>) {
    use tokio::signal::unix::{SignalKind, signal};

    let (mut term, mut int) = match (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) {
        (Ok(term), Ok(int)) => (term, int),
        (term, int) => {
            error!(
                "could not listen for SIGTERM/SIGINT (term: {:?}, int: {:?}); the broker will not \
                 shut down in an orderly way when Kubernetes asks it to",
                term.err(),
                int.err()
            );
            return;
        }
    };

    tokio::spawn(async move {
        loop {
            let name = tokio::select! {
                _ = term.recv() => "SIGTERM",
                _ = int.recv() => "SIGINT",
            };
            info!("received {name}, beginning the orderly shutdown");
            if broker
                .cast(BrokerMessage::PrepareForShutdown {
                    auth_token: token.clone(),
                })
                .is_err()
            {
                // The broker is gone; nothing left to ask.
                return;
            }
        }
    });
}
