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
    Command, JIRA_USER_OBSERVER, JiraError, JiraObserverArgs, JiraObserverMessage,
    JiraObserverState, publish_data,
};
use jira_common::JIRA_USERS_CONSUMER_TOPIC;
use jira_common::types::{JiraData, JiraUser};
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use std::time::Duration;
use tracing::{debug, info};

const USERS_OP: &str = "/rest/api/2/user/picker";

pub struct JiraUserObserver;

impl JiraUserObserver {
    fn tick() -> JiraObserverMessage {
        JiraObserverMessage::Tick(Command::GetUsers(USERS_OP.to_string()))
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

    /// Pulls every page of users and publishes them together.
    async fn pull(op: &str, state: &mut JiraObserverState) -> Result<(), ActorProcessingErr> {
        state.ensure_token().await?;

        let mut all_users = Vec::new();
        let mut start_at = 0;
        let max_results = 50;
        let query_string = "''";

        loop {
            let url = format!(
                "{}{}?query={}&startAt={}&maxResults={}",
                state.jira_url, op, query_string, start_at, max_results
            );
            debug!("{url}");

            let res: serde_json::Value = state.get_json(&url).await?;
            debug!("User picker response: {res}");

            let users: Vec<JiraUser> = if res["users"].is_null() {
                Vec::new()
            } else {
                serde_json::from_value(res["users"].clone()).map_err(|e| {
                    JiraError::new(format!("could not decode the users from {op}: {e}"))
                })?
            };
            let total = res["total"].as_u64().unwrap_or(0);
            let fetched = users.len();

            all_users.extend(users);

            // An empty page ends the walk: with a non-zero
            // total it would otherwise never advance.
            if fetched == 0 || (start_at + fetched) >= total as usize {
                break;
            }

            start_at += fetched;
        }

        publish_data(
            &state.tcp_client,
            JIRA_USERS_CONSUMER_TOPIC,
            &JiraData::Users(all_users),
        )
        .await
    }
}

#[async_trait]
impl Actor for JiraUserObserver {
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
        JiraUserObserver::observe(&myself, state)
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
                if let Command::GetUsers(op) = command {
                    let pulled = JiraUserObserver::pull(&op, state).await;
                    state.settle(&myself, JIRA_USER_OBSERVER, pulled, Self::tick)?;
                }
            }
        }
        Ok(())
    }
}
