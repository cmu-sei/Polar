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
use crate::{
    Command, JIRA_GROUP_OBSERVER, JiraError, JiraObserverArgs, JiraObserverMessage,
    JiraObserverState, publish_data,
};
use jira_common::JIRA_GROUPS_CONSUMER_TOPIC;
use jira_common::types::{JiraData, JiraGroup};
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use std::time::Duration;
use tracing::{debug, info};

const GROUPS_OP: &str = "/rest/api/2/groups/picker";

pub struct JiraGroupObserver;

impl JiraGroupObserver {
    fn tick() -> JiraObserverMessage {
        JiraObserverMessage::Tick(Command::GetGroups(GROUPS_OP.to_string()))
    }

    fn observe(
        myself: &ActorRef<JiraObserverMessage>,
        state: &JiraObserverState,
    ) -> Result<(), ActorProcessingErr> {
        info!("Observing every {} seconds", state.base_interval.as_secs());

        // Poll straight away rather than leaving the graph empty for a
        // whole interval after every start. Each pull schedules the next
        // (`JiraObserverState::settle`): an interval away after a success,
        // sooner after a failure.
        myself.cast(Self::tick())?;
        Ok(())
    }

    /// Pulls every page of groups and publishes them together.
    async fn pull(op: &str, state: &mut JiraObserverState) -> Result<(), ActorProcessingErr> {
        state.ensure_token().await?;
        debug!("{op}");

        let mut all_groups = Vec::new();
        let mut start_at = 0;
        let max_results = 50;
        let query_string = "";

        loop {
            let url = format!(
                "{}{}?query={}&startAt={}&maxResults={}",
                state.jira_url, op, query_string, start_at, max_results
            );

            let res: serde_json::Value = state.get_json(&url).await?;
            debug!("Group Data {}", res["groups"]);

            let groups: Vec<JiraGroup> = if res["groups"].is_null() {
                Vec::new()
            } else {
                serde_json::from_value(res["groups"].clone()).map_err(|e| {
                    JiraError::new(format!("could not decode the groups from {op}: {e}"))
                })?
            };
            let total = res["total"].as_u64().unwrap_or(0);
            let fetched = groups.len();

            all_groups.extend(groups);

            // An empty page ends the walk: with a non-zero
            // total it would otherwise never advance.
            if fetched == 0 || (start_at + fetched) >= total as usize {
                break;
            }

            start_at += fetched;
        }

        publish_data(
            &state.tcp_client,
            JIRA_GROUPS_CONSUMER_TOPIC,
            &JiraData::Groups(all_groups),
        )
        .await
    }
}

#[async_trait]
impl Actor for JiraGroupObserver {
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
        )
        .with_retry(args.retry, args.clock))
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        JiraGroupObserver::observe(&myself, state)
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            JiraObserverMessage::Resync => {}
            JiraObserverMessage::Tick(command) => {
                if let Command::GetGroups(op) = command {
                    let pulled = JiraGroupObserver::pull(&op, state).await;
                    state.settle(&myself, JIRA_GROUP_OBSERVER, pulled, Self::tick)?;
                }
            }
        }
        Ok(())
    }
}
