/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/
use crate::{
    handle_backoff, Command, JiraObserverArgs, JiraObserverMessage, JiraObserverState,
    BROKER_CLIENT_NAME,
};
use cassini_client::TcpClientMessage;
use cassini_types::ClientMessage;
use jira_common::types::{JiraData, JiraProject};
use jira_common::JIRA_PROJECTS_CONSUMER_TOPIC;
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use rkyv::rancor::Error;
use std::time::Duration;
use tracing::{debug, info};

pub struct JiraProjectObserver;

impl JiraProjectObserver {
    fn observe(
        myself: ActorRef<JiraObserverMessage>,
        state: &mut JiraObserverState,
        _duration: Duration,
    ) {
        info!(
            "Observing every {} seconds",
            state.backoff_interval.as_secs()
        );

        let handle = myself
            .send_interval(state.backoff_interval, || {
                //build query
                let op = "/rest/api/2/project";
                // pass query in message
                JiraObserverMessage::Tick(Command::GetProjects(op.to_string()))
            })
            .abort_handle();

        state.task_handle = Some(handle);
    }
}
#[async_trait]
impl Actor for JiraProjectObserver {
    type Msg = JiraObserverMessage;
    type State = JiraObserverState;
    type Arguments = JiraObserverArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: JiraObserverArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to instance");

        let state = JiraObserverState::new(
            args.jira_url,
            args.token,
            args.web_client,
            args.registration_id,
            Duration::from_secs(args.base_interval),
            Duration::from_secs(args.max_backoff),
        );
        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        JiraProjectObserver::observe(myself, state, state.base_interval);
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            JiraObserverMessage::Tick(command) => {
                match command {
                    Command::GetProjects(op) => {
                        debug!("{}", op.to_string());

                        let res = state
                            .web_client
                            .get(format!("{}{}", state.jira_url, op))
                            .bearer_auth(format!("{}", state.token.clone().expect("TOKEN")))
                            .send()
                            .await?
                            .json::<Vec<JiraProject>>()
                            .await?;

                        let tcp_client = where_is(BROKER_CLIENT_NAME.to_string())
                            .expect("Expected to find client");

                        let data = JiraData::Projects(res.clone());
                        println!("{:#?}", data);
                        let bytes = rkyv::to_bytes::<Error>(&data).unwrap();

                        let msg = ClientMessage::PublishRequest {
                            topic: JIRA_PROJECTS_CONSUMER_TOPIC.to_string(),
                            payload: bytes.to_vec(),
                            registration_id: state.registration_id.clone(),
                        };

                        // Serialize the inner client message before sending
                        let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&msg)
                            .expect("Failed to serialize ClientMessage::PublishRequest");

                        tcp_client
                            .send_message(TcpClientMessage::Publish {
                                topic: JIRA_PROJECTS_CONSUMER_TOPIC.to_string(),
                                payload: payload.into_vec(),
                            })
                            .expect("Expected to publish message");
                    }
                    _ => (),
                }
            }
            JiraObserverMessage::Backoff(reason) => {
                // cancel old event loop and start a new one with updated state, if observer hasn't stopped
                if let Some(handle) = &state.task_handle {
                    handle.abort();
                    // start new loop
                    match handle_backoff(state, reason) {
                        Ok(duration) => {
                            JiraProjectObserver::observe(myself, state, duration);
                        }
                        Err(e) => myself.stop(Some(e.to_string())),
                    }
                }
            }
        }
        Ok(())
    }
}
