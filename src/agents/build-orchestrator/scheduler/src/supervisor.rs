//! SchedulerSupervisor — ticks, queries the graph, publishes requests.

use crate::{
    IN_FLIGHT_QUERY, PENDING_PACKAGES_QUERY, RecentRequests, SERVICE_NAME, SchedulerConfig,
    analysis_request, requests_allowed,
};
use cassini_types::ClientEvent;
use neo4rs::{Graph, query};
use orchestrator_core::types::subjects::BUILD_REQUESTS_TOPIC;
use polar::cassini::{CassiniClient, PublishRequest, TcpClient};
use polar::get_neo_config;
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, info, warn};

const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

pub enum SchedulerMsg {
    Client(ClientEvent),
    /// Self-scheduled on `config.interval` once registered with Cassini.
    Tick,
    PrepareShutdown,
    ForceExit,
}

pub struct SchedulerState {
    config: SchedulerConfig,
    graph: Graph,
    broker: Arc<dyn CassiniClient>,
    healthcheck: ActorRef<HealthCheckMessage>,
    recent: RecentRequests,
    ticking: bool,
    draining: bool,
}

pub struct SchedulerSupervisor;

impl SchedulerSupervisor {
    /// One scheduling pass. Errors are reported to the healthcheck and logged;
    /// the next tick simply tries again.
    async fn tick(state: &mut SchedulerState) {
        let rows = match Self::in_flight(state).await {
            Ok(in_flight) => {
                let limit = requests_allowed(
                    state.config.batch_size,
                    state.config.max_in_flight,
                    in_flight,
                );
                if limit == 0 {
                    debug!(in_flight, "at in-flight limit — not requesting this tick");
                    let _ = state.healthcheck.cast(HealthCheckMessage::GraphAvailable);
                    return;
                }
                Self::pending_packages(state, limit).await
            }
            Err(e) => Err(e),
        };
        let rows = match rows {
            Ok(rows) => {
                let _ = state.healthcheck.cast(HealthCheckMessage::GraphAvailable);
                rows
            }
            Err(e) => {
                warn!(error = %e, "pending-package query failed");
                let _ = state
                    .healthcheck
                    .cast(HealthCheckMessage::GraphOpFailed(e.to_string()));
                return;
            }
        };

        let mut published = 0;
        let now = Instant::now();
        for (purl, sha256) in rows {
            if !state.recent.admit(&purl, now) {
                continue;
            }
            let request = match analysis_request(&purl, &sha256, &state.config.analyzer_image) {
                Ok(r) => r,
                Err(e) => {
                    // Recorded by admit() above, so this is logged once per TTL.
                    warn!(purl = %purl, error = %e, "cannot build analysis request — skipping");
                    continue;
                }
            };
            let payload = match serde_json::to_vec(&request) {
                Ok(p) => p,
                Err(e) => {
                    error!(purl = %purl, error = %e, "failed to serialize request");
                    continue;
                }
            };
            match state.broker.publish(PublishRequest {
                topic: BUILD_REQUESTS_TOPIC.to_string(),
                payload,
                trace_ctx: None,
                offline_behavior: Default::default(),
            }) {
                Ok(()) => {
                    published += 1;
                    debug!(purl = %purl, build_id = %request.build_id, "analysis requested");
                }
                Err(e) => error!(purl = %purl, error = %e, "failed to publish request"),
            }
        }
        info!(published, "scheduling pass complete");
    }

    async fn in_flight(state: &SchedulerState) -> Result<i64, neo4rs::Error> {
        let mut result = state
            .graph
            .execute(
                query(IN_FLIGHT_QUERY).param("analyzer_image", state.config.analyzer_image.clone()),
            )
            .await?;
        Ok(match result.next().await? {
            Some(row) => row.get::<i64>("n").unwrap_or(0),
            None => 0,
        })
    }

    async fn pending_packages(
        state: &SchedulerState,
        limit: i64,
    ) -> Result<Vec<(String, String)>, neo4rs::Error> {
        let mut result = state
            .graph
            .execute(
                query(PENDING_PACKAGES_QUERY)
                    .param("analyzer_image", state.config.analyzer_image.clone())
                    .param("max_failed_attempts", state.config.max_failed_attempts)
                    .param("limit", limit),
            )
            .await?;
        let mut rows = Vec::new();
        while let Some(row) = result.next().await? {
            match (row.get::<String>("purl"), row.get::<String>("sha256")) {
                (Ok(purl), Ok(sha256)) => rows.push((purl, sha256)),
                (purl, _) => warn!(purl = ?purl.ok(), "skipping malformed package row"),
            }
        }
        Ok(rows)
    }

    fn begin_drain(myself: &ActorRef<SchedulerMsg>, state: &mut SchedulerState) {
        if state.draining {
            return;
        }
        state.draining = true;
        info!("entering drain window before exiting for rejuvenation");
        myself.send_after(
            ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS),
            || SchedulerMsg::ForceExit,
        );
    }
}

#[async_trait]
impl Actor for SchedulerSupervisor {
    type Msg = SchedulerMsg;
    type State = SchedulerState;
    type Arguments = SchedulerConfig;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        config: SchedulerConfig,
    ) -> Result<Self::State, ActorProcessingErr> {
        info!(analyzer_image = %config.analyzer_image, "analysis scheduler starting");

        let prepare_shutdown_port = Arc::new(OutputPort::<()>::default());
        prepare_shutdown_port.subscribe(myself.clone(), |()| Some(SchedulerMsg::PrepareShutdown));

        let dep_cert_endpoints = vec![
            DepCertEndpoint::parse("cassini-ip-svc.polar.svc.cluster.local:8080:300")
                .map_err(ActorProcessingErr::from)?,
            DepCertEndpoint::parse("polar-db-svc.polar-graph.svc.cluster.local:7687:300")
                .map_err(ActorProcessingErr::from)?,
        ];
        let (healthcheck, _) = HealthCheckActor::spawn_linked(
            Some(HEALTHCHECK_ACTOR_NAME.to_string()),
            HealthCheckActor,
            HealthCheckArgs {
                expects_graph: true,
                rejuvenation_threshold_secs: 300,
                dep_cert_endpoints,
                prepare_shutdown_port,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        // connect() builds pool config only; GraphAvailable is reported after
        // the first successful query.
        let graph = Graph::connect(get_neo_config()?)?;
        let _ = healthcheck.cast(HealthCheckMessage::GraphConnected);

        let broker = Arc::new(
            TcpClient::spawn(&format!("{SERVICE_NAME}.tcp"), myself.clone(), |event| {
                Some(SchedulerMsg::Client(event))
            })
            .await?,
        );

        Ok(SchedulerState {
            recent: RecentRequests::new(config.request_ttl),
            config,
            graph,
            broker,
            healthcheck,
            ticking: false,
            draining: false,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SchedulerMsg::Client(ClientEvent::Registered { .. }) => {
                let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);
                // Registered fires again after a reconnect; start ticking once.
                if !state.ticking {
                    state.ticking = true;
                    myself.send_interval(state.config.interval, || SchedulerMsg::Tick);
                    let _ = myself.cast(SchedulerMsg::Tick);
                }
            }
            SchedulerMsg::Client(ClientEvent::TransportError { reason }) => {
                warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                let _ = state
                    .healthcheck
                    .cast(HealthCheckMessage::CassiniDisconnected);
            }
            SchedulerMsg::Client(_) => {}
            SchedulerMsg::Tick => {
                if !state.draining {
                    Self::tick(state).await;
                }
            }
            SchedulerMsg::PrepareShutdown => {
                info!("PrepareShutdown received");
                if let Err(e) = state.healthcheck.cast(HealthCheckMessage::ShutdownAck) {
                    error!("failed to send ShutdownAck: {e}");
                }
            }
            SchedulerMsg::ForceExit => {
                warn!("drain window elapsed, exiting now");
                std::process::exit(1);
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                warn!(actor = ?cell.get_name(), ?reason, "child actor terminated");
                if cell.get_name().as_deref() == Some(HEALTHCHECK_ACTOR_NAME) {
                    Self::begin_drain(&myself, state);
                }
            }
            SupervisionEvent::ActorFailed(cell, e) => {
                error!(actor = ?cell.get_name(), error = %e, "child actor failed");
                if cell.get_name().as_deref() == Some(HEALTHCHECK_ACTOR_NAME) {
                    Self::begin_drain(&myself, state);
                }
            }
            _ => {}
        }
        Ok(())
    }
}
