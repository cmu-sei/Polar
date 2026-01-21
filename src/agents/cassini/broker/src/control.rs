use cassini_types::{BrokerMessage, ControlError, ControlOp, ControlResult};
use ractor::{concurrency::Duration, rpc::CallResult, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, trace_span, warn};
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
                let span = trace_span!("ControlManager.handle_control_request", %registration_id);
                trace_ctx.map(|ctx| span.set_parent(ctx).ok());

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
                        let span = trace_span!("ControlManager Received ListSessionsDirective");
                        let _ = span.enter();

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
                                CallResult::Success(sessions) => BrokerMessage::ControlResponse {
                                    registration_id,
                                    result: Ok(ControlResult::SessionList(sessions)),
                                    trace_ctx: Some(span.context()),
                                },
                                CallResult::SenderError => BrokerMessage::ControlResponse {
                                    registration_id,
                                    result: Err(ControlError::InternalError(
                                        "Operation failed".to_string(),
                                    )),
                                    trace_ctx: Some(span.context()),
                                },
                                CallResult::Timeout => BrokerMessage::ControlResponse {
                                    registration_id,
                                    result: Err(ControlError::InternalError(
                                        "Operation timed out.".to_string(),
                                    )),
                                    trace_ctx: Some(span.context()),
                                },
                            },
                            Err(_e) => todo!(),
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
                        let span = trace_span!("ControlManager Received ListTopics Directive");
                        let _ = span.enter();

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
                                CallResult::Success(topics) => BrokerMessage::ControlResponse {
                                    registration_id,
                                    result: Ok(ControlResult::TopicList(topics)),
                                    trace_ctx: Some(span.context()),
                                },
                                CallResult::SenderError => BrokerMessage::ControlResponse {
                                    registration_id,
                                    result: Err(ControlError::InternalError(
                                        "Operation failed".to_string(),
                                    )),
                                    trace_ctx: Some(span.context()),
                                },
                                CallResult::Timeout => BrokerMessage::ControlResponse {
                                    registration_id,
                                    result: Err(ControlError::InternalError(
                                        "Operation timed out.".to_string(),
                                    )),
                                    trace_ctx: Some(span.context()),
                                },
                            },
                            Err(_e) => todo!(),
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

                //fowrard response back to client
                reply_to.map(|session| {
                    session
                        .cast(resp)
                        .map_err(|_e| warn!("Failed to respond to session!"))
                        .ok()
                });
            }
            _ => warn!("{UNEXPECTED_MESSAGE_STR}"),
        }
        Ok(())
    }
}
