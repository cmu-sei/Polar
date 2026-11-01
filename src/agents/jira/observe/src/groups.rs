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
use reqwest::Url;
use std::collections::HashSet;
use std::time::Duration;
use tracing::{debug, info, warn};

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
    ///
    /// The group picker takes no `startAt` (a Data Center ignores one and answers the first page again), and answers
    /// at most its autocomplete limit (`jira.ajax.autocomplete.limit`) of groups whatever `maxResults` asks; what it
    /// takes is `exclude`, the groups to leave out. So each page asks again leaving out every group already read,
    /// until the picker has none left or as many have been read as it said match. Each group read is in the next
    /// request's address, so a site with very many groups is read only as far as [`MAX_URL_LEN`] allows, logged.
    ///
    /// A page that fails leaves what was read in the observer's state ([`GroupWalk`]), and the next attempt goes on
    /// from that page.
    async fn pull(op: &str, state: &mut JiraObserverState) -> Result<(), ActorProcessingErr> {
        state.ensure_token().await?;
        debug!("{op}");

        let mut walk = state.group_walk.take().unwrap_or_default();
        if !walk.groups.is_empty() {
            info!(
                "{op}: going on from the {} groups already read",
                walk.groups.len()
            );
        }
        if let Err(e) = Self::walk(op, state, &mut walk).await {
            state.group_walk = Some(walk);
            return Err(e);
        }

        publish_data(
            &state.tcp_client,
            JIRA_GROUPS_CONSUMER_TOPIC,
            &JiraData::Groups(walk.groups),
        )
        .await
    }

    async fn walk(
        op: &str,
        state: &JiraObserverState,
        walk: &mut GroupWalk,
    ) -> Result<(), ActorProcessingErr> {
        let endpoint = format!("{}{}", state.jira_url, op);
        loop {
            let mut params = vec![
                ("query".to_string(), String::new()),
                ("maxResults".to_string(), PAGE_SIZE.to_string()),
            ];
            params.extend(
                walk.groups
                    .iter()
                    .map(|g| ("exclude".to_string(), g.name.clone())),
            );
            let url = Url::parse_with_params(&endpoint, params)?;
            if url.as_str().len() > MAX_URL_LEN {
                warn!(
                    "{op}: read {} groups and stopped: leaving out that many more makes the request longer than \
                     {MAX_URL_LEN} bytes; the rest are not read",
                    walk.groups.len()
                );
                return Ok(());
            }

            let res: serde_json::Value = state.get_json(url.as_str()).await?;
            let groups: Vec<JiraGroup> = if res["groups"].is_null() {
                Vec::new()
            } else {
                serde_json::from_value(res["groups"].clone()).map_err(|e| {
                    JiraError::new(format!("could not decode the groups from {op}: {e}"))
                })?
            };
            let total = res["total"].as_u64();
            if total.is_none() && !walk.warned_no_total {
                warn!(
                    "{op} answered without a total: reading until it has no group left that was not read"
                );
                walk.warned_no_total = true;
            }

            // A page with nothing new ends the walk: a site that ignored `exclude` would otherwise answer the same
            // page for ever.
            let before = walk.groups.len();
            for group in groups {
                if walk.seen.insert(group.name.clone()) {
                    walk.groups.push(group);
                }
            }
            debug!("{op}: read {} of {total:?} groups", walk.groups.len());
            if walk.groups.len() == before
                || total.is_some_and(|total| walk.groups.len() as u64 >= total)
            {
                return Ok(());
            }
        }
    }
}

/// The groups read so far by a walk of the picker that has not finished.
#[derive(Default)]
pub struct GroupWalk {
    groups: Vec<JiraGroup>,
    seen: HashSet<String>,
    warned_no_total: bool,
}

/// How many groups a page asks for; the picker answers at most its autocomplete limit, which may be fewer.
const PAGE_SIZE: usize = 50;

/// The longest request the group walk sends, in bytes: within the 8 KiB request line and headers a Tomcat (and so a
/// Data Center) accepts by default, with room for the headers.
pub const MAX_URL_LEN: usize = 6 * 1024;

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
