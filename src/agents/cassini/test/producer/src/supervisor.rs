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

//! The producer's supervisor: the builder gives it the health check and the Cassini client, and once the client has
//! registered it runs the plan, once. The client's acknowledgements and refusals are counted where they arrive, in
//! the lifecycle handler, so the loop sees them without a message to this actor. When the run is done it logs the
//! result and asks for an orderly shutdown; it exits 0 only then.

use crate::sending::{Plan, Progress, send_all};
use crate::settings::ProducerSettings;
use cassini_client::{OfflineBehavior, PublishRequest};
use cassini_types::ClientEvent;
use harness_common::envelope::{encode, now_ns, payload};
use harness_common::report::{ProducerReport, Report};
use harness_common::schedule::window;
use harness_common::settings::Run;
use polar::cassini::CassiniClient;
use polar_agent_builder::{
    AgentBuilder, Configured, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage,
};
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

/// The workload this is: its catalog workload's name and its certificate's.
pub const SERVICE_NAME: &str = "harness-producer";

/// Nothing here has anything to finish before it stops.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);

pub enum ProducerMessage {
    Registered,
    Disconnected(String),
    Finished(Box<Report>),
    PrepareShutdown,
}

pub struct ProducerSupervisor;

pub struct ProducerState {
    healthcheck: HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    client: polar::cassini::TcpClient,
    run: Run,
    settings: ProducerSettings,
    progress: Arc<Progress>,
    sending: Option<tokio::task::JoinHandle<()>>,
    done: bool,
}

#[async_trait]
impl Actor for ProducerSupervisor {
    type Msg = ProducerMessage;
    type State = ProducerState;
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        let run = Run::read(configured.config())?;
        let settings = ProducerSettings::read(configured.config())?;
        let progress = Arc::new(Progress::default());
        let counted = progress.clone();
        let topic = run.topic();
        let resources = AgentBuilder::new(myself.clone(), SERVICE_NAME)
            .with_lifecycle_handler(move |event| match event {
                ClientEvent::Registered { .. } => Some(ProducerMessage::Registered),
                ClientEvent::PublishAcknowledged { topic: t } if t == topic => {
                    counted.acknowledge();
                    None
                }
                ClientEvent::PublishRefused { topic: t, reason } if t == topic => {
                    warn!(%reason, "a message was refused");
                    counted.refuse();
                    None
                }
                ClientEvent::TransportError { reason } => {
                    Some(ProducerMessage::Disconnected(reason))
                }
                _ => None,
            })
            .into_configured(configured)
            .build()
            .await?;
        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => Some(ProducerMessage::PrepareShutdown),
            });
        info!(
            "{SERVICE_NAME} {} of run `{}`: {} message(s) of {} bytes on `{topic}` at {} a second",
            run.index, run.id, run.count, settings.size, settings.rate
        );
        Ok(ProducerState {
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            client: resources.tcp_client,
            run,
            settings,
            progress,
            sending: None,
            done: false,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ProducerMessage::Registered => {
                let _ = state.healthcheck.cassini_connected();
                // A reconnect does not start the run again.
                if state.sending.is_none() && !state.done {
                    state.sending = Some(tokio::spawn(produce(
                        myself,
                        state.client.clone(),
                        state.run.clone(),
                        Plan {
                            count: state.run.count,
                            rate: state.settings.rate,
                            window: window(state.settings.window_bytes, state.settings.size),
                            stall: state.settings.stall,
                        },
                        state.settings.size,
                        state.progress.clone(),
                    )));
                }
            }
            ProducerMessage::Disconnected(reason) => {
                warn!("Transport error (non-fatal, awaiting reconnect): {reason}");
                let _ = state.healthcheck.cassini_disconnected();
            }
            ProducerMessage::Finished(report) => {
                info!("{}", report.line());
                state.done = true;
                let _ = state
                    .infrastructure
                    .cast(InfrastructureSupervisorMessage::BeginShutdown {
                        reason: "the run is done".to_string(),
                    });
            }
            ProducerMessage::PrepareShutdown => {
                if let Some(sending) = state.sending.take() {
                    sending.abort();
                }
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

/// Runs the plan and tells the supervisor what came of it.
async fn produce(
    supervisor: ActorRef<ProducerMessage>,
    client: polar::cassini::TcpClient,
    run: Run,
    plan: Plan,
    size: u64,
    progress: Arc<Progress>,
) {
    let body = payload(size as usize);
    let topic = run.topic();
    let producer = run.index as u32;
    let sent = send_all(plan, &progress, |seqno| {
        client
            .publish(PublishRequest {
                topic: topic.to_string(),
                payload: encode(producer, seqno, now_ns(), &body),
                trace_ctx: None,
                offline_behavior: OfflineBehavior::Queue,
                share_identity: true,
            })
            .is_ok()
    })
    .await;
    let report = Report::Producer(ProducerReport {
        run: run.id,
        index: run.index,
        topic: topic.to_string(),
        size,
        rate: plan.rate,
        count: plan.count,
        window: plan.window,
        sent: sent.sent,
        acknowledged: progress.acknowledged(),
        refused: progress.refused(),
        failed: sent.failed,
        elapsed_secs: sent.total.as_secs_f64(),
        achieved_rate: if sent.sending.is_zero() {
            0.0
        } else {
            sent.sent as f64 / sent.sending.as_secs_f64()
        },
    });
    if sent.gave_up {
        warn!(
            "nothing was acknowledged for {:?}; reporting what was",
            plan.stall
        );
    }
    let _ = supervisor.cast(ProducerMessage::Finished(Box::new(report)));
}
