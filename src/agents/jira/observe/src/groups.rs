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
use crate::{Command, JiraObserverArgs, JiraObserverMessage, JiraObserverState, publish_data};
use jira_common::JIRA_GROUPS_CONSUMER_TOPIC;
use jira_common::types::{JiraData, JiraGroup};
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use std::time::Duration;
use tracing::{debug, info};

pub struct JiraGroupObserver;

impl JiraGroupObserver {
    fn observe(myself: &ActorRef<JiraObserverMessage>, state: &JiraObserverState) {
        info!("Observing every {} seconds", state.base_interval.as_secs());

        // The interval task ends on its own once this actor has stopped and
        // the cast fails, so there is no handle to keep.
        myself.send_interval(state.base_interval, || {
            JiraObserverMessage::Tick(Command::GetGroups("/rest/api/2/groups/picker".to_string()))
        });
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
        ))
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        JiraGroupObserver::observe(&myself, state);
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            JiraObserverMessage::Tick(command) => {
                if let Command::GetGroups(op) = command {
                    debug!("{}", op.to_string());

                    let mut all_groups = Vec::new();
                    let mut start_at = 0;
                    let max_results = 50;
                    let query_string = "";

                    loop {
                        let url = format!(
                            "{}{}?query={}&startAt={}&maxResults={}",
                            state.jira_url, op, query_string, start_at, max_results
                        );

                        let res = state
                            .auth
                            .apply(state.web_client.get(&url))
                            .send()
                            .await?
                            .json::<serde_json::Value>()
                            .await?;
                        debug!("Group Data {}", res["groups"].to_string());
                        let groups: Vec<JiraGroup> = if res["groups"].is_null() {
                            Vec::new()
                        } else {
                            serde_json::from_value(res["groups"].clone())?
                        };
                        let total = res["total"].as_u64().unwrap_or(0);
                        let fetched = groups.len();

                        all_groups.extend(groups);

                        if (start_at + fetched) >= total as usize {
                            break;
                        }

                        start_at += fetched;
                    }

                    publish_data(
                        &state.tcp_client,
                        JIRA_GROUPS_CONSUMER_TOPIC,
                        &JiraData::Groups(all_groups),
                    )?;
                }
            }
        }
        Ok(())
    }
}
