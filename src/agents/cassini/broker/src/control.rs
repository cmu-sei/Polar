use cassini_types::{BrokerMessage, ControlError, ControlOp, ControlResult};
use ractor::{concurrency::Duration, rpc::CallResult, Actor, ActorProcessingErr, ActorRef};
use std::time::Instant;
use tracing::{debug, error, trace_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::UNEXPECTED_MESSAGE_STR;

// ========================
// ControlManager Actor
// ========================

pub struct ControlManager;

pub struct ControlManagerState {
    pub listener_mgr: ActorRef<BrokerMessage>,
    pub session_mgr: ActorRef<BrokerMessage>,
    pub topic_mgr: ActorRef<BrokerMessage>,
    pub subscriber_mgr: ActorRef<BrokerMessage>,
}

#[ractor::async_trait]
impl Actor for ControlManager {
    type Msg = BrokerMessage;
    type State = ControlManagerState;
    type Arguments = ControlManagerState;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(args)
    }

    async fn post_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            BrokerMessage::ControlRequest {
                registration_id,
                op,
                trace_ctx,
                reply_to,
            } => {
                // Parent span for the whole control request; attach upstream ctx if present.
                let span = trace_span!(
                    "cassini.control_manager.handle",
                    %registration_id,
                    op = ?op
                );
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
                let _g = span.enter();

                let resp = match op {
                    ControlOp::Ping => {
                        debug!("Control Manager Received Ping");
                        BrokerMessage::ControlResponse {
                            registration_id,
                            result: Ok(ControlResult::Pong),
                            trace_ctx: Some(span.context()),
                        }
                    }
                    ControlOp::ListSessions => {
                        // Make this span a *child* of the request span (not a new root).
                        let span = trace_span!(
                            parent: &span,
                            "cassini.control_manager.list_sessions",
                            timeout_ms = 100u64
                        );
                        let _g = span.enter();
                        let t0 = Instant::now();

                        match state
                            .session_mgr
                            .call(
                                |reply_to| BrokerMessage::GetSessions {
                                    reply_to,
                                    trace_ctx: Some(span.context()),
                                },
                                Some(Duration::from_millis(100)),
                            )
                            .await
                        {
                            Ok(result) => match result {
                                CallResult::Success(sessions) => {
                                    debug!(elapsed_ms = t0.elapsed().as_millis(), sessions = sessions.len(), "list_sessions ok");
                                    BrokerMessage::ControlResponse {

                                    registration_id,
                                    result: Ok(ControlResult::SessionList(sessions)),
                                    trace_ctx: Some(span.context()),
                                    }
                                }
                                CallResult::SenderError => {
                                    warn!(elapsed_ms = t0.elapsed().as_millis(), "list_sessions sender_error");
                                    BrokerMessage::ControlResponse {
                                    registration_id,
                                    result: Err(ControlError::InternalError(
                                        "Operation failed".to_string(),
                                    )),
                                    trace_ctx: Some(span.context()),
                                    }
                                }
                                CallResult::Timeout => {
                                    warn!(elapsed_ms = t0.elapsed().as_millis(), "list_sessions timeout");
                                    BrokerMessage::ControlResponse {
                                    registration_id,
                                    result: Err(ControlError::InternalError(
                                        "Operation timed out.".to_string(),
                                    )),
                                    trace_ctx: Some(span.context()),
                                    }
                                }
                            },
                            Err(e) => {
                                // ractor::rpc::CallError: actor stopped/mailbox closed/etc.
                                error!(elapsed_ms = t0.elapsed().as_millis(), error = %e, "session_mgr call failed");
                                BrokerMessage::ControlResponse {
                                    registration_id,
                                    result: Err(ControlError::InternalError(
                                        "Session manager unavailable".to_string(),
                                    )),
                                    trace_ctx: Some(span.context()),
                                }
                            }
                        }
                    }
                    // ControlOp::GetSessionInfo { registration_id } => {
                    //     let sessions = state.sessions.lock().await;
                    //     if let Some(s) = sessions.get(&registration_id) {
                    //         Ok(ControlResult::SessionInfo(s.clone()))
                    //     } else {
                    //         Err(ControlError::NotFound(registration_id))
                    //     }
                    // }
                    // TODO: Is this something we'd want to support? eventually, admins might want something like this so they can cut off sessions at will.
                    // ControlOp::DisconnectSession { registration_id } => {
                    //     let mut sessions = state.sessions.lock().await;
                    //     if sessions.remove(&registration_id).is_some() {
                    //         Ok(ControlResult::Disconnected)
                    //     } else {
                    //         Err(ControlError::NotFound(registration_id))
                    //     }
                    // }
                    ControlOp::ListTopics => {
                        let span = trace_span!(
                            parent: &span,
                            "cassini.control_manager.list_topics",
                            timeout_ms = 100u64
                        );
                        let _g = span.enter();
                        let t0 = Instant::now();

                        match state
                            .topic_mgr
                            .call(
                                |reply_to| BrokerMessage::GetTopics {
                                    registration_id: registration_id.clone(),
                                    reply_to,
                                    trace_ctx: Some(span.context()),
                                },
                                Some(Duration::from_millis(100)),
                            )
                            .await
                        {
                            Ok(result) => match result {
                                CallResult::Success(topics) => {
                                    debug!(elapsed_ms = t0.elapsed().as_millis(), topics = topics.len(), "list_topics ok");
                                    BrokerMessage::ControlResponse {
                                    registration_id,
                                    result: Ok(ControlResult::TopicList(topics)),
                                    trace_ctx: Some(span.context()),
                                    }
                                }
                                CallResult::SenderError => {
                                    warn!(elapsed_ms = t0.elapsed().as_millis(), "list_topics sender_error");
                                    BrokerMessage::ControlResponse {
                                    registration_id,
                                    result: Err(ControlError::InternalError(
                                        "Operation failed".to_string(),
                                    )),
                                    trace_ctx: Some(span.context()),
                                    }
                                }
                                CallResult::Timeout => {
                                    warn!(elapsed_ms = t0.elapsed().as_millis(), "list_topics timeout");
                                    BrokerMessage::ControlResponse {
                                    registration_id,
                                    result: Err(ControlError::InternalError(
                                        "Operation timed out.".to_string(),
                                    )),
                                    trace_ctx: Some(span.context()),
                                    }
                                }
                            },
                            Err(e) => {
                                error!(elapsed_ms = t0.elapsed().as_millis(), error = %e, "topic_mgr call failed");
                                BrokerMessage::ControlResponse {
                                    registration_id,
                                    result: Err(ControlError::InternalError(
                                        "Topic manager unavailable".to_string(),
                                    )),
                                    trace_ctx: Some(span.context()),
                                }
                            }
                        }
                    }
                    // ControlOp::ListSubscribers { topic } => {
                    //     let topics = state.topics.lock().await;
                    //     if let Some(subs) = topics.get(&topic) {
                    //         Ok(ControlResult::SubscriberList(subs.clone()))
                    //     } else {
                    //         Err(ControlError::NotFound(topic))
                    //     }
                    // }
                    _ => todo!("Handle other options"),
                };

                // forward response back to client
                if let Some(session) = reply_to {
                    if let Err(e) = session.cast(resp) {
                        warn!(error = %e, "Failed to respond to session");
                    } else {
                        // Helpful breadcrumb when correlating "request -> response" without opening every span.
                        debug!("ControlResponse forwarded to session");
                    }
                } else {
                    warn!("ControlRequest missing reply_to; dropping response");
                }
            }
            other => {
                warn!(?other, "{UNEXPECTED_MESSAGE_STR}");
            }
        }
        Ok(())
    }
}
