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

//! What an agent that writes to the graph does around its graph controller, which every such agent
//! did by hand: start the controller from the builder's snapshot of the environment with its
//! signals going to the supervisor, stop it waiting for a write in flight, and run the drain that
//! precedes the exit or the handover to the infrastructure supervisor.

use crate::infrastructure::InfrastructureSupervisorMessage;
use polar::graph::controller::{
    GraphControllerActor, GraphControllerArgs, GraphControllerMsg, GraphSignal,
};
use polar::graph::environment::GraphSettings;
use ractor::concurrency::Duration;
use ractor::{Actor, ActorProcessingErr, ActorRef, Message, OutputPort};
use std::sync::Arc;
use tracing::{error, info, warn};

/// Starts the graph controller as a child of `supervisor`, reading the connection from the
/// builder's snapshot. Each signal it raises is turned into the supervisor's own message by `wrap`.
/// The controller only builds pool configuration here; `GraphAvailable` is what proves
/// connectivity, so it does not wait for Cassini.
pub async fn spawn_graph_controller<M: Message>(
    supervisor: &ActorRef<M>,
    name: String,
    config: &polar_env::Config,
    wrap: fn(GraphSignal) -> M,
) -> Result<ActorRef<GraphControllerMsg>, ActorProcessingErr> {
    let port = Arc::new(OutputPort::<GraphSignal>::default());
    port.subscribe(supervisor.clone(), move |signal| Some(wrap(signal)));
    let settings = GraphSettings::from_env(config).map_err(ActorProcessingErr::from)?;
    let (controller, _) = Actor::spawn_linked(
        Some(name),
        GraphControllerActor,
        GraphControllerArgs::new(port).with_settings(settings),
        supervisor.get_cell(),
    )
    .await
    .map_err(ActorProcessingErr::from)?;
    Ok(controller)
}

/// Stops the controller, waiting up to `wait` for a write in flight. A controller that does not
/// stop in time is logged, not waited on: the process is about to end.
pub async fn stop_graph_controller(controller: ActorRef<GraphControllerMsg>, wait: Duration) {
    match controller
        .stop_and_wait(Some("supervisor shutting down".into()), Some(wait))
        .await
    {
        Ok(()) => info!("graph controller stopped cleanly"),
        Err(e) => error!(
            "graph controller did not stop cleanly within {}s, exiting anyway: {e:?}",
            wait.as_secs()
        ),
    }
}

/// Where a supervisor is in coming down. Messages that arrive while `draining` are logged and not
/// processed, since whatever failed (likely the graph controller) cannot be trusted.
#[derive(Debug, Default)]
pub struct Drain {
    draining: bool,
    rejuvenating: bool,
}

impl Drain {
    pub fn is_draining(&self) -> bool {
        self.draining
    }

    /// True when the drain was begun by `PrepareShutdown`, so its end completes the handshake
    /// instead of exiting directly.
    pub fn is_rejuvenating(&self) -> bool {
        self.rejuvenating
    }

    /// Begins the drain: after `window`, `elapsed` is delivered to `supervisor`. A drain already
    /// under way is left alone, since its exit is pending.
    pub fn begin<M: Message>(
        &mut self,
        supervisor: &ActorRef<M>,
        window: Duration,
        rejuvenating: bool,
        elapsed: fn() -> M,
    ) -> bool {
        if self.draining {
            return false;
        }
        self.draining = true;
        self.rejuvenating = rejuvenating;
        let _ = supervisor.send_after(window, elapsed);
        true
    }

    /// The end of the drain, once the supervisor has stopped its own children: after
    /// `PrepareShutdown` the infrastructure supervisor is told to proceed, otherwise the process
    /// exits 1 so the Job starts a fresh pod.
    pub fn complete(&self, infrastructure: &ActorRef<InfrastructureSupervisorMessage>) {
        if self.rejuvenating {
            info!("domain shutdown complete, releasing infrastructure");
            if let Err(e) =
                infrastructure.cast(InfrastructureSupervisorMessage::ProceedWithShutdown)
            {
                error!("could not tell infrastructure to proceed ({e}); exiting now");
                std::process::exit(1);
            }
        } else {
            warn!("exiting now");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Probe;
    enum Msg {
        Elapsed,
    }

    #[ractor::async_trait]
    impl Actor for Probe {
        type Msg = Msg;
        type State = Arc<Mutex<u32>>;
        type Arguments = Arc<Mutex<u32>>;
        async fn pre_start(
            &self,
            _: ActorRef<Msg>,
            seen: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(seen)
        }
        async fn handle(
            &self,
            _: ActorRef<Msg>,
            _: Msg,
            seen: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            *seen.lock().unwrap() += 1;
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_drain_begins_once_and_delivers_one_elapsed() {
        let seen = Arc::new(Mutex::new(0));
        let (probe, _) = Actor::spawn(None, Probe, seen.clone()).await.unwrap();
        let mut drain = Drain::default();
        assert!(!drain.is_draining());
        assert!(drain.begin(&probe, Duration::from_millis(20), true, || Msg::Elapsed));
        assert!(drain.is_draining() && drain.is_rejuvenating());
        // A second begin, as a failure after PrepareShutdown would make, changes nothing.
        assert!(!drain.begin(&probe, Duration::from_millis(20), false, || Msg::Elapsed));
        assert!(drain.is_rejuvenating());
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert_eq!(*seen.lock().unwrap(), 1);
    }
}
