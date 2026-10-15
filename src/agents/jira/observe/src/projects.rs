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
use crate::{Command, JiraObserverArgs, JiraObserverMessage, JiraObserverState, publish_data};
use jira_common::JIRA_PROJECTS_CONSUMER_TOPIC;
use jira_common::types::{JiraData, JiraProject};
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use std::time::Duration;
use tracing::{debug, info};

const PROJECTS_OP: &str = "/rest/api/2/project";

pub struct JiraProjectObserver;

impl JiraProjectObserver {
    fn tick() -> JiraObserverMessage {
        JiraObserverMessage::Tick(Command::GetProjects(PROJECTS_OP.to_string()))
    }

    fn observe(
        myself: &ActorRef<JiraObserverMessage>,
        state: &JiraObserverState,
    ) -> Result<(), ActorProcessingErr> {
        info!("Observing every {} seconds", state.base_interval.as_secs());

        // Poll straight away rather than leaving the graph empty for a
        // whole interval after every start. The interval task ends on its
        // own once this actor has stopped and the cast fails, so there is
        // no handle to keep.
        myself.cast(Self::tick())?;
        myself.send_interval(state.base_interval, Self::tick);
        Ok(())
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

        Ok(JiraObserverState::new(
            args.jira_url,
            args.auth,
            args.deployment,
            args.web_client,
            args.tcp_client,
            Duration::from_secs(args.base_interval),
        ))
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        JiraProjectObserver::observe(&myself, state)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            JiraObserverMessage::Tick(command) => {
                if let Command::GetProjects(op) = command {
                    debug!("{op}");

                    let projects: Vec<JiraProject> =
                        state.get_json(&format!("{}{}", state.jira_url, op)).await?;

                    publish_data(
                        &state.tcp_client,
                        JIRA_PROJECTS_CONSUMER_TOPIC,
                        &JiraData::Projects(projects),
                    )?;
                }
            }
        }
        Ok(())
    }
}
