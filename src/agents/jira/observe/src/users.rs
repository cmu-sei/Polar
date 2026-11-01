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
    Command, JIRA_USER_OBSERVER, JiraDeployment, JiraError, JiraObserverArgs, JiraObserverMessage,
    JiraObserverState, publish_data,
};
use jira_common::JIRA_USERS_CONSUMER_TOPIC;
use jira_common::types::{JiraData, JiraUser};
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use reqwest::Url;
use std::collections::HashSet;
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
    ///
    /// On a Data Center or Server, the user search (`/rest/api/2/user/search`), where `username=.` matches every
    /// user, active and inactive, paged by `startAt`: the user picker it replaces takes no `startAt` and answers at
    /// most its autocomplete limit, and `query=''` matches no one. On Cloud, which has no such search by name, the
    /// picker as before. Either is read to an empty page, or one with no user not already read (a site that ignored
    /// `startAt`).
    ///
    /// A page that fails leaves what was read in the observer's state ([`UserWalk`]), and the next attempt goes on
    /// from that page.
    async fn pull(op: &str, state: &mut JiraObserverState) -> Result<(), ActorProcessingErr> {
        state.ensure_site().await?;
        state.ensure_token().await?;

        let mut walk = state.user_walk.take().unwrap_or_default();
        if walk.start_at > 0 {
            info!("going on from user {} of the walk", walk.start_at);
        }
        if let Err(e) = Self::walk(op, state, &mut walk).await {
            state.user_walk = Some(walk);
            return Err(e);
        }

        publish_data(
            &state.tcp_client,
            JIRA_USERS_CONSUMER_TOPIC,
            &JiraData::Users(walk.users),
        )
        .await
    }

    async fn walk(
        op: &str,
        state: &JiraObserverState,
        walk: &mut UserWalk,
    ) -> Result<(), ActorProcessingErr> {
        loop {
            let (endpoint, mut params) = match state.deployment {
                JiraDeployment::ServerOrDataCenter => (
                    USER_SEARCH_OP,
                    vec![
                        ("username", ".".to_string()),
                        ("includeActive", "true".to_string()),
                        ("includeInactive", "true".to_string()),
                    ],
                ),
                JiraDeployment::Cloud => (op, vec![("query", "''".to_string())]),
            };
            params.push(("startAt", walk.start_at.to_string()));
            params.push(("maxResults", PAGE_SIZE.to_string()));
            let url = Url::parse_with_params(&format!("{}{endpoint}", state.jira_url), params)?;
            debug!("{url}");

            let res: serde_json::Value = state.get_json(url.as_str()).await?;
            // The search answers a list; the picker an object holding one.
            let list = if res.is_array() {
                res
            } else {
                res["users"].clone()
            };
            let users: Vec<JiraUser> = if list.is_null() {
                Vec::new()
            } else {
                serde_json::from_value(list).map_err(|e| {
                    JiraError::new(format!("could not decode the users from {endpoint}: {e}"))
                })?
            };
            let fetched = users.len();

            let mut new = 0usize;
            for user in users {
                // One the processor cannot key is passed on for it to say so, but it is not progress: a site that
                // ignored `startAt` would otherwise be asked for ever.
                match user.identity().map(str::to_string) {
                    Some(identity) => {
                        if walk.seen.insert(identity) {
                            new += 1;
                            walk.users.push(user);
                        }
                    }
                    None => walk.users.push(user),
                }
            }
            if fetched == 0 || new == 0 {
                return Ok(());
            }
            walk.start_at += fetched;
        }
    }
}

/// The users read so far by a walk that has not finished, and where it goes on from.
#[derive(Default)]
pub struct UserWalk {
    users: Vec<JiraUser>,
    seen: HashSet<String>,
    start_at: usize,
}

/// A Data Center's and Server's search for users by name, paged by `startAt`.
const USER_SEARCH_OP: &str = "/rest/api/2/user/search";

/// How many users a page asks for.
const PAGE_SIZE: usize = 50;

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
