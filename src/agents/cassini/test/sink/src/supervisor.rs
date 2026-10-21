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

//! The sink's supervisor: the builder gives it the health check, the Cassini client and the subscription to its lane.
//! Each message is stamped with its arrival where it is delivered and handed here to be measured. Once a second it
//! looks whether the run is over for it; when it is, it logs the result and asks for an orderly shutdown, and exits 0
//! only then.

use crate::measure::Measure;
use crate::settings::SinkSettings;
use cassini_types::ClientEvent;
use harness_common::envelope::{Stamp, now_ns, read};
use harness_common::lanes::{lane_of, producers_on};
use harness_common::report::{Report, ready_line};
use harness_common::settings::Run;
use polar_agent_builder::{
    AgentBuilder, Configured, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage, Subscription,
};
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

/// The workload this is: its catalog workload's name and its certificate's.
pub const SERVICE_NAME: &str = "harness-sink";

/// Nothing here has anything to finish before it stops.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);

/// How often the sink looks whether the run is over for it.
const TICK: Duration = Duration::from_secs(1);

pub enum SinkMessage {
    Arrived {
        stamp: Stamp,
        at_ns: u64,
        at: Instant,
    },
    Registered,
    Subscribed,
    Disconnected(String),
    Tick,
    PrepareShutdown,
}

pub struct SinkSupervisor;

pub struct SinkState {
    healthcheck: HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    run: Run,
    settings: SinkSettings,
    measure: Measure,
    ready: Option<Instant>,
    done: bool,
}

impl SinkState {
    fn finish(&mut self, ended: &str) {
        if self.done {
            return;
        }
        self.done = true;
        info!(
            "{}",
            Report::Sink(self.measure.report(&self.run, ended)).line()
        );
        let _ = self
            .infrastructure
            .cast(InfrastructureSupervisorMessage::BeginShutdown {
                reason: "the run is done".to_string(),
            });
    }
}

#[async_trait]
impl Actor for SinkSupervisor {
    type Msg = SinkMessage;
    type State = SinkState;
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        let run = Run::read(configured.config())?;
        let settings = SinkSettings::read(configured.config())?;
        let topic = run.topic();
        let producers = producers_on(lane_of(run.index, run.lanes), run.producers, run.lanes);
        let delivered = myself.clone();
        let resources = AgentBuilder::new(myself.clone(), SERVICE_NAME)
            .with_subscription(Subscription::new(topic, move |incoming| {
                let (at_ns, at) = (now_ns(), Instant::now());
                match read(&incoming.payload) {
                    Ok(stamp) => {
                        let _ = delivered.cast(SinkMessage::Arrived { stamp, at_ns, at });
                    }
                    Err(e) => warn!("not a harness message: {e}"),
                }
            }))
            .with_lifecycle_handler(move |event| match event {
                ClientEvent::Registered { .. } => Some(SinkMessage::Registered),
                ClientEvent::ClientSubscribed { topic: t } if t == topic => {
                    Some(SinkMessage::Subscribed)
                }
                ClientEvent::SubscribeRefused { topic: t, reason } if t == topic => {
                    error!(%reason, "the broker refused this sink's subscription");
                    None
                }
                ClientEvent::TransportError { reason } => Some(SinkMessage::Disconnected(reason)),
                _ => None,
            })
            .into_configured(configured)
            .build()
            .await?;
        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => Some(SinkMessage::PrepareShutdown),
            });
        drop(myself.send_interval(TICK, || SinkMessage::Tick));
        info!(
            "{SERVICE_NAME} {} of run `{}`: {} message(s) from each of producers {producers:?} on `{topic}`",
            run.index, run.id, run.count
        );
        Ok(SinkState {
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            measure: Measure::new(producers, run.count, settings.warmup),
            run,
            settings,
            ready: None,
            done: false,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SinkMessage::Arrived { stamp, at_ns, at } => {
                if !state.done {
                    state.measure.receive(stamp, at_ns, at);
                    if state.measure.complete() {
                        state.finish("complete");
                    }
                }
            }
            SinkMessage::Registered => {
                let _ = state.healthcheck.cassini_connected();
            }
            SinkMessage::Subscribed => {
                if state.ready.is_none() {
                    state.ready = Some(Instant::now());
                    info!(
                        "{}",
                        ready_line(&state.run.id, state.run.index, state.run.topic())
                    );
                    // A lane no producer is on has nothing to wait for.
                    if state.measure.complete() {
                        state.finish("complete");
                    }
                }
            }
            SinkMessage::Disconnected(reason) => {
                warn!("Transport error (non-fatal, awaiting reconnect): {reason}");
                let _ = state.healthcheck.cassini_disconnected();
            }
            SinkMessage::Tick => {
                let waited = match (state.measure.last(), state.ready) {
                    (Some(last), _) => last.elapsed() > state.settings.idle,
                    (None, Some(ready)) => ready.elapsed() > state.settings.first_wait,
                    (None, None) => false,
                };
                if waited {
                    state.finish("idle");
                }
            }
            SinkMessage::PrepareShutdown => {
                let _ = state
                    .infrastructure
                    .cast(InfrastructureSupervisorMessage::ProceedWithShutdown);
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisionEvent::ActorTerminated(actor, _, reason) => {
                if actor.get_id() == state.infrastructure.get_id() {
                    info!("infrastructure shutdown complete ({reason:?}), exiting");
                    // A Job: done is a success; anything else ending it is not.
                    std::process::exit(if state.done { 0 } else { 1 });
                }
            }
            SupervisionEvent::ActorFailed(actor, err) => {
                error!("Child actor {actor:?} failed: {err}; exiting");
                std::process::exit(1);
            }
            _ => {}
        }
        Ok(())
    }
}
