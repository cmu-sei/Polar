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

//! What the git processor is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.

use crate::SERVICE_NAME;
use crate::supervisor::{GitProcessorSupervisor, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError, TopicDecl};
use ractor::Actor;
use std::time::Duration;

pub struct GitProcessor;

impl PolarAgent for GitProcessor {
    const NAME: &'static str = SERVICE_NAME;
    const PURPOSE: &'static str =
        "Records the commits and ref updates the git observer publishes in the graph";
    const NEEDS: &'static [Capability] = &[Capability::GraphWrite];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const TOPICS: &'static [TopicDecl] = &[
        TopicDecl::subscribe(
            polar::topics::GIT_REPOSITORY_EVENTS,
            "the observer's commits and ref updates, written to the graph",
        ),
        TopicDecl::subscribe(
            git_agent_common::status::GIT_REPO_OBSERVATION_STATUS,
            "the observer's status for each repository, written to the graph",
        ),
        TopicDecl::subscribe(
            git_agent_common::proposal::PROPOSAL_DECISIONS_TOPIC,
            "the controller's decisions, written to the graph",
        ),
    ];
    const HELP: Option<&'static str> = Some(
        "The git processor subscribes to the repository events the git observer publishes \
         (polar.git.repositories.events). Each discovered commit becomes a commit node in its \
         repository with edges to its parents; each ref update adds an edge from the ref to the \
         commit it now points at, and earlier edges stay as the ref's history. It needs the \
         graph (GRAPH_*).",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(SERVICE_NAME);
        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{SERVICE_NAME}.supervisor")),
            GitProcessorSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the git processor's supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the git processor's supervisor task failed: {e}"))?;
        // A processor that is no longer processing must be restarted.
        Err("the git processor's supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(GitProcessor);
}
